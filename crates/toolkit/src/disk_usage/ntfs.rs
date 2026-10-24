//! Assemble a logical directory tree from one read-only NTFS MFT pass.
#[path = "ntfs_records.rs"]
mod ntfs_records;
#[path = "ntfs_volume.rs"]
mod ntfs_volume;

use super::{CancelToken, DiskNode, DiskUsage, ScanError, ScanMethod, ScanProgress};
use ntfs_records::{Name, Record};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::OsString,
    io,
    path::Path,
    time::{Duration, Instant},
};

const REFERENCE_MASK: u64 = (1 << 48) - 1;
const SKIP_ATTRIBUTES: u32 = 0x400 | 0x1000 | 0x40000 | 0x400000;

struct Entry {
    base: u64,
    names: Vec<Name>,
    bytes: Option<u64>,
    is_directory: bool,
    attributes: u32,
}

#[derive(Default)]
struct Collection {
    entries: HashMap<u64, Entry>,
    error: Option<io::Error>,
    cancelled: bool,
    records: u64,
    total: u64,
}
impl Collection {
    fn insert(&mut self, record: Record) {
        if self.error.is_some() {
            return;
        }
        if record.number > REFERENCE_MASK {
            self.error = Some(invalid("NTFS record number exceeds a file reference"));
            return;
        }
        let reference = record.number | (u64::from(record.sequence) << 48);
        let entry = Entry {
            base: record.base_reference,
            names: record.names,
            bytes: record.data_size,
            is_directory: record.is_directory,
            attributes: record.attributes,
        };
        // In particular, data run arrays are not retained for ordinary records.
        if self.entries.insert(reference, entry).is_some() {
            self.error = Some(invalid("Duplicate NTFS file reference during MFT scan"));
        }
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(super) fn scan(
    root: &Path,
    cancel: &CancelToken,
    progress: &mut impl FnMut(ScanProgress),
) -> io::Result<DiskUsage> {
    let started = Instant::now();
    let mut collection = Collection::default();
    let mut total = 0;
    let summary = ntfs_volume::read(
        root,
        cancel,
        |record| collection.insert(record),
        |records, count| {
            total = count;
            progress(ScanProgress {
                method: ScanMethod::NtfsMft,
                records,
                total_records: count,
                stage: "Reading MFT",
                elapsed: started.elapsed(),
                ..Default::default()
            });
        },
    )?;
    collection.records = summary.records_read;
    collection.total = total;
    collection.cancelled = summary.cancelled;
    assemble(
        root,
        summary.root_reference,
        collection,
        cancel,
        progress,
        started,
    )
}

struct Link {
    reference: u64,
    name: OsString,
    bytes: u64,
    is_directory: bool,
    attributes: u32,
}

fn issue(result: &mut DiskUsage, message: impl FnOnce() -> String) {
    result.nodes[0].errors += 1;
    if result.errors.len() < 32 {
        result.errors.push(ScanError {
            path: result.root_path.clone(),
            message: message(),
        });
    }
}

fn assemble(
    root: &Path,
    root_reference: u64,
    collection: Collection,
    cancel: &CancelToken,
    progress: &mut impl FnMut(ScanProgress),
    started: Instant,
) -> io::Result<DiskUsage> {
    if let Some(error) = collection.error {
        return Err(error);
    }
    let mut entries = collection.entries;
    let cancelled = collection.cancelled || cancel.is_cancelled();
    if !cancelled
        && !entries
            .get(&root_reference)
            .is_some_and(|entry| entry.base == 0 && entry.is_directory)
    {
        return Err(invalid(
            "The NTFS root record was not present in the MFT snapshot",
        ));
    }
    let mut result = DiskUsage {
        root_path: root.to_owned(),
        nodes: vec![DiskNode {
            name: root.as_os_str().to_owned(),
            parent: None,
            children: Vec::new(),
            is_directory: true,
            bytes: 0,
            files: 0,
            directories: 0,
            skipped: 0,
            errors: 0,
        }],
        cancelled,
        elapsed: Duration::ZERO,
        errors: Vec::new(),
        method: ScanMethod::NtfsMft,
        fallback_reason: None,
    };
    let mut totals = ScanProgress {
        method: ScanMethod::NtfsMft,
        records: collection.records,
        total_records: collection.total,
        stage: "Building tree",
        elapsed: started.elapsed(),
        ..Default::default()
    };
    progress(totals);
    if cancelled || cancel.is_cancelled() {
        result.cancelled = true;
        result.elapsed = started.elapsed();
        totals.elapsed = result.elapsed;
        totals.stage = "Cancelled";
        progress(totals);
        return Ok(result);
    }
    // Split extension records out before merging; a base reference always names
    // a base record, never another extension. Exact references include sequence
    // numbers so a deleted/reused parent cannot steal a stale child.
    let extensions: Vec<_> = entries
        .iter()
        .filter_map(|(&reference, entry)| (entry.base != 0).then_some(reference))
        .collect();
    for reference in extensions {
        if cancel.is_cancelled() {
            break;
        }
        let mut extension = entries.remove(&reference).expect("collected extension");
        match entries.get_mut(&extension.base) {
            Some(base) if base.base == 0 => {
                if let Some(bytes) = extension.bytes {
                    if base.bytes.is_some_and(|existing| existing != bytes) {
                        return Err(invalid(
                            "Conflicting NTFS unnamed data sizes in extension records",
                        ));
                    }
                    base.bytes = Some(bytes);
                }
                for name in extension.names.drain(..) {
                    if !base.names.contains(&name) {
                        base.names.push(name);
                    }
                }
                if extension.attributes != 0 {
                    if base.attributes != 0 && base.attributes != extension.attributes {
                        return Err(invalid(
                            "Conflicting NTFS standard information in extension records",
                        ));
                    }
                    base.attributes = extension.attributes;
                }
            }
            _ => issue(&mut result, || {
                format!(
                    "NTFS extension record {} has no matching base reference",
                    reference & REFERENCE_MASK
                )
            }),
        }
    }
    let directories: HashSet<_> = entries
        .iter()
        .filter_map(|(&reference, entry)| entry.is_directory.then_some(reference))
        .collect();
    let mut links: HashMap<u64, Vec<Link>> = HashMap::new();
    for (reference, entry) in entries {
        if cancel.is_cancelled() {
            break;
        }
        for name in entry.names {
            // The root's dot name is a self-reference, not a child entry.
            if reference == root_reference
                && name.parent_reference == root_reference
                && name.name == "."
            {
                continue;
            }
            if !directories.contains(&name.parent_reference) {
                issue(&mut result, || {
                    format!(
                        "NTFS record {} has a missing or stale directory reference",
                        reference & REFERENCE_MASK
                    )
                });
                continue;
            }
            links.entry(name.parent_reference).or_default().push(Link {
                reference,
                name: name.name,
                bytes: entry.bytes.unwrap_or(0),
                is_directory: entry.is_directory,
                attributes: entry.attributes,
            });
        }
    }
    drop(directories);
    let mut pending = VecDeque::from([(root_reference, 0)]);
    let mut visited = HashSet::from([root_reference]);
    let mut excluded = Vec::new();
    let mut last_update = Instant::now();
    while let Some((reference, parent)) = pending.pop_front() {
        if cancel.is_cancelled() {
            break;
        }
        let Some(mut children) = links.remove(&reference) else {
            continue;
        };
        children.sort_unstable_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| a.reference.cmp(&b.reference))
        });
        let before = children.len();
        children.dedup_by(|a, b| a.name == b.name);
        for _ in children.len()..before {
            issue(&mut result, || {
                "Two NTFS records described the same directory entry".into()
            });
        }
        for child in children {
            if cancel.is_cancelled() {
                break;
            }
            if child.attributes & SKIP_ATTRIBUTES != 0 {
                result.nodes[parent].skipped += 1;
                totals.skipped += 1;
                if child.is_directory {
                    excluded.push(child.reference);
                }
                continue;
            }
            if child.is_directory && !visited.insert(child.reference) {
                issue(&mut result, || {
                    format!(
                        "NTFS directory {} is linked more than once or forms a cycle",
                        child.reference & REFERENCE_MASK
                    )
                });
                continue;
            }
            let id = result.nodes.len();
            let bytes = if child.is_directory { 0 } else { child.bytes };
            result.nodes.push(DiskNode {
                name: child.name,
                parent: Some(parent),
                children: Vec::new(),
                is_directory: child.is_directory,
                bytes,
                files: u64::from(!child.is_directory),
                directories: 0,
                skipped: 0,
                errors: 0,
            });
            result.nodes[parent].children.push(id);
            if child.is_directory {
                totals.directories += 1;
                pending.push_back((child.reference, id));
            } else {
                totals.files += 1;
                totals.bytes = totals.bytes.saturating_add(bytes);
            }
            if last_update.elapsed() >= Duration::from_millis(100) {
                totals.errors = result.nodes[0].errors;
                totals.elapsed = started.elapsed();
                progress(totals);
                last_update = Instant::now();
            }
        }
    }
    result.cancelled = cancel.is_cancelled();
    if !result.cancelled {
        // Discard excluded subtrees without labeling intentionally omitted cloud
        // or reparse descendants as filesystem corruption.
        while let Some(reference) = excluded.pop() {
            if let Some(children) = links.remove(&reference) {
                excluded.extend(
                    children
                        .into_iter()
                        .filter_map(|child| child.is_directory.then_some(child.reference)),
                );
            }
        }
        for children in links.into_values() {
            for child in children {
                issue(&mut result, || {
                    format!(
                        "NTFS record {} is unreachable from the volume root",
                        child.reference & REFERENCE_MASK
                    )
                });
            }
        }
    }
    super::finish(&mut result, cancel);
    result.cancelled |= cancel.is_cancelled();
    result.elapsed = started.elapsed();
    totals.elapsed = result.elapsed;
    totals.errors = result.nodes[0].errors;
    totals.stage = if result.cancelled {
        "Cancelled"
    } else {
        "Complete"
    };
    progress(totals);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reference(number: u64) -> u64 {
        number | (1 << 48)
    }
    fn record(number: u64, parent: u64, name: &str, directory: bool, bytes: Option<u64>) -> Record {
        Record {
            number,
            sequence: 1,
            base_reference: 0,
            is_directory: directory,
            attributes: 0,
            names: vec![Name {
                parent_reference: parent,
                name: name.into(),
            }],
            data_size: bytes,
            data_runs: None,
            data_initialized: bytes,
            has_attribute_list: false,
            attribute_list: None,
            attribute_list_runs: None,
            attribute_list_size: None,
            attribute_list_initialized: None,
            data_extents: Vec::new(),
        }
    }
    fn collection(records: impl IntoIterator<Item = Record>) -> Collection {
        let mut collection = Collection::default();
        collection.insert(record(5, reference(5), ".", true, None));
        for record in records {
            collection.insert(record);
        }
        collection
    }
    fn build(collection: Collection) -> io::Result<DiskUsage> {
        assemble(
            Path::new("C:\\"),
            reference(5),
            collection,
            &CancelToken::new(),
            &mut |_| {},
            Instant::now(),
        )
    }

    #[test]
    fn merges_extension_sizes_and_names_and_counts_file_links_per_path() {
        let mut extension = record(100, reference(5), "alias.bin", false, Some(95));
        extension.base_reference = reference(10);
        extension.names.push(Name {
            parent_reference: reference(5),
            name: "first.bin".into(),
        });
        let mut file = record(10, reference(5), "first.bin", false, None);
        file.names.push(Name {
            parent_reference: reference(20),
            name: "nested-link.bin".into(),
        });
        let tree = build(collection([
            file,
            extension,
            record(20, reference(5), "folder", true, None),
            record(30, reference(20), "small.bin", false, Some(7)),
        ]))
        .unwrap();
        assert_eq!(
            (
                tree.nodes[0].bytes,
                tree.nodes[0].files,
                tree.nodes[0].directories,
                tree.nodes[0].errors
            ),
            (292, 4, 1, 0)
        );
        assert_eq!(tree.nodes.len(), 6);
        for (id, node) in tree.nodes.iter().enumerate().skip(1) {
            assert!(node.parent.unwrap() < id);
        }
        let folder = tree
            .nodes
            .iter()
            .find(|node| node.name == "folder")
            .unwrap();
        assert_eq!((folder.bytes, folder.files), (102, 2));
    }

    #[test]
    fn skips_junction_subtrees_and_reports_stale_parents_and_orphan_cycles() {
        let mut junction = record(10, reference(5), "junction", true, None);
        junction.attributes = 0x400;
        let stale = 5 | (2 << 48);
        let tree = build(collection([
            junction,
            record(11, reference(10), "omitted", false, Some(100)),
            record(12, stale, "stale", false, Some(100)),
            record(20, reference(21), "cycle-a", true, None),
            record(21, reference(20), "cycle-b", true, None),
            record(30, reference(99), "orphan", false, Some(100)),
            record(40, reference(5), "empty", false, None),
        ]))
        .unwrap();
        assert_eq!(
            (
                tree.nodes[0].bytes,
                tree.nodes[0].files,
                tree.nodes[0].skipped,
                tree.nodes[0].errors
            ),
            (0, 1, 1, 4)
        );
        assert_eq!(tree.nodes.len(), 2);
        assert_eq!(tree.errors.len(), 4);
    }

    #[test]
    fn contradictory_stream_sizes_require_fallback() {
        let mut extension = record(100, reference(5), "file", false, Some(9));
        extension.base_reference = reference(10);
        assert!(
            build(collection([
                record(10, reference(5), "file", false, Some(5)),
                extension
            ]))
            .is_err()
        );
    }

    #[test]
    fn cancellation_keeps_a_valid_prefix_and_reports_cancelled_stage() {
        let cancel = CancelToken::new();
        let mut stages = Vec::new();
        let tree = assemble(
            Path::new("C:\\"),
            reference(5),
            collection([record(10, reference(5), "file", false, Some(10))]),
            &cancel,
            &mut |progress| {
                stages.push(progress.stage);
                cancel.cancel();
            },
            Instant::now(),
        )
        .unwrap();
        assert!(tree.cancelled);
        assert_eq!(tree.nodes.len(), 1);
        assert_eq!(tree.nodes[0].parent, None);
        assert_eq!(stages.last(), Some(&"Cancelled"));
    }

    #[test]
    fn missing_root_and_duplicate_references_are_errors() {
        assert!(build(Collection::default()).is_err());
        assert!(
            build(collection([
                record(10, reference(5), "one", false, Some(1)),
                record(10, reference(5), "two", false, Some(1))
            ]))
            .is_err()
        );
    }
}
