//! Read-only NTFS MFT streaming. No writes, volume locks, or per-file opens.
use super::ntfs_records::{self, Record, Run};
use rcr_workflows::workflow::CancelToken;
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    mem::{offset_of, size_of},
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
    path::Path,
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::HANDLE,
    Storage::FileSystem::{
        FILE_FLAG_NO_BUFFERING, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    },
    System::{
        IO::DeviceIoControl,
        Ioctl::{
            FSCTL_GET_NTFS_FILE_RECORD, FSCTL_GET_NTFS_VOLUME_DATA, NTFS_FILE_RECORD_INPUT_BUFFER,
            NTFS_FILE_RECORD_OUTPUT_BUFFER, NTFS_VOLUME_DATA_BUFFER,
        },
    },
};

const CHUNK_BYTES: usize = 8 * 1024 * 1024;
const REFERENCE_MASK: u64 = (1 << 48) - 1;
const MAX_ATTRIBUTE_LIST_BYTES: usize = 1024 * 1024;
const MAX_BOOTSTRAP_RECORDS: usize = 1024;
const MAX_BOOTSTRAP_RUNS: usize = 32768;

pub(super) struct VolumeSummary {
    pub(super) root_reference: u64,
    pub(super) cancelled: bool,
    /// Record slots read, including unused/deleted slots.
    pub(super) records_read: u64,
}

struct Geometry {
    sector: usize,
    cluster: u64,
    record: usize,
    initialized: u64,
    mft_lcn: u64,
    clusters: u64,
    volume_bytes: u64,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn unsupported(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

fn device_path(root: &Path) -> io::Result<String> {
    let path: Vec<_> = root.as_os_str().encode_wide().collect();
    let path = path.strip_prefix(&[92, 92, 63, 92]).unwrap_or(&path);
    if path.len() != 3
        || !matches!(path[0], 65..=90 | 97..=122)
        || path[1] != 58
        || !matches!(path[2], 47 | 92)
    {
        return Err(unsupported(
            "Fast NTFS scanning currently requires a local drive root, such as C:\\",
        ));
    }
    Ok(format!(
        r"\\.\{}:",
        char::from_u32(u32::from(path[0]))
            .unwrap()
            .to_ascii_uppercase()
    ))
}

fn windows_error(error: windows::core::Error) -> io::Error {
    let code = error.code().0 as u32;
    if code & 0xffff_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((code & 0xffff) as i32)
    } else {
        io::Error::other(error.to_string())
    }
}

fn geometry(file: &File) -> io::Result<Geometry> {
    let mut data = NTFS_VOLUME_DATA_BUFFER::default();
    let mut returned = 0;
    // SAFETY: the synchronous call borrows a live File handle and a correctly
    // sized, initialized output object for the complete duration of the call.
    unsafe {
        DeviceIoControl(
            HANDLE(file.as_raw_handle()),
            FSCTL_GET_NTFS_VOLUME_DATA,
            None,
            0,
            Some((&mut data as *mut NTFS_VOLUME_DATA_BUFFER).cast()),
            size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
            Some(&mut returned),
            None,
        )
    }
    .map_err(windows_error)?;
    if returned < size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32 {
        return Err(invalid("NTFS volume geometry response was truncated."));
    }
    let sector = data.BytesPerSector as usize;
    let cluster = u64::from(data.BytesPerCluster);
    let record = data.BytesPerFileRecordSegment as usize;
    if !matches!(sector, 512 | 4096)
        || !cluster.is_power_of_two()
        || cluster < sector as u64
        || cluster > 2 * 1024 * 1024
        || !record.is_power_of_two()
        || !(1024..=65536).contains(&record)
        || data.NumberSectors <= 0
        || data.TotalClusters <= 0
        || data.MftStartLcn < 0
        || data.MftValidDataLength <= 0
    {
        return Err(unsupported(
            "Unsupported NTFS sector, cluster, or file-record geometry.",
        ));
    }
    let volume_bytes = (data.NumberSectors as u64)
        .checked_mul(sector as u64)
        .ok_or_else(|| invalid("NTFS volume size overflow."))?;
    let clusters = data.TotalClusters as u64;
    if clusters
        .checked_mul(cluster)
        .is_none_or(|bytes| bytes > volume_bytes)
        || data.MftStartLcn as u64 >= clusters
    {
        return Err(invalid("NTFS volume geometry exceeds the volume bounds."));
    }
    Ok(Geometry {
        sector,
        cluster,
        record,
        initialized: data.MftValidDataLength as u64,
        mft_lcn: data.MftStartLcn as u64,
        clusters,
        volume_bytes,
    })
}

fn check_bootstrap(file: &File, record_bytes: usize) -> io::Result<()> {
    let input = NTFS_FILE_RECORD_INPUT_BUFFER {
        FileReferenceNumber: 0,
    };
    let header = offset_of!(NTFS_FILE_RECORD_OUTPUT_BUFFER, FileRecordBuffer);
    let mut output = vec![0u8; size_of::<NTFS_FILE_RECORD_OUTPUT_BUFFER>() + record_bytes];
    let mut returned = 0;
    // SAFETY: both buffers remain live for this synchronous METHOD_BUFFERED
    // request; their lengths match the input structure and allocated output.
    unsafe {
        DeviceIoControl(
            HANDLE(file.as_raw_handle()),
            FSCTL_GET_NTFS_FILE_RECORD,
            Some((&input as *const NTFS_FILE_RECORD_INPUT_BUFFER).cast()),
            size_of::<NTFS_FILE_RECORD_INPUT_BUFFER>() as u32,
            Some(output.as_mut_ptr().cast()),
            output.len() as u32,
            Some(&mut returned),
            None,
        )
    }
    .map_err(windows_error)?;
    if (returned as usize) < header + record_bytes {
        return Err(invalid(
            "NTFS bootstrap file-record response was truncated.",
        ));
    }
    let reference = u64::from_le_bytes(output[0..8].try_into().unwrap());
    let length = u32::from_le_bytes(output[8..12].try_into().unwrap()) as usize;
    if reference & REFERENCE_MASK != 0
        || length != record_bytes
        || output.get(header..header + 4) != Some(b"FILE".as_slice())
    {
        return Err(invalid(
            "NTFS bootstrap response did not contain MFT record zero.",
        ));
    }
    Ok(())
}

/// Sector-aligned storage without custom allocation or unsafe slice creation.
struct ReadBuffer {
    storage: Vec<u8>,
    offset: usize,
}

impl ReadBuffer {
    fn new(sector: usize) -> Self {
        // 512e disks can report 512-byte logical sectors over 4 KiB physical
        // sectors. Keep the buffer page-aligned for their unbuffered I/O too.
        let alignment = sector.max(4096);
        let storage = vec![0u8; CHUNK_BYTES + alignment];
        let offset = (alignment - storage.as_ptr() as usize % alignment) % alignment;
        Self { storage, offset }
    }

    fn bytes(&mut self, len: usize) -> &mut [u8] {
        &mut self.storage[self.offset..self.offset + len]
    }
}

fn read_at(file: &mut File, offset: u64, output: &mut [u8], sector: usize) -> io::Result<()> {
    if !offset.is_multiple_of(sector as u64)
        || !output.len().is_multiple_of(sector)
        || !(output.as_ptr() as usize).is_multiple_of(sector)
    {
        return Err(invalid("Unaligned NTFS volume read."));
    }
    file.seek(SeekFrom::Start(offset))?;
    let read = file.read(output)?;
    if read != output.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "NTFS volume read returned an incomplete block.",
        ));
    }
    Ok(())
}

fn validate_mapping(runs: &[Run], geometry: &Geometry, contiguous: bool) -> io::Result<u64> {
    if runs.is_empty() {
        return Err(invalid("MFT has no data runs."));
    }
    let mut next_vcn = 0u64;
    let mut physical_ranges = Vec::with_capacity(runs.len());
    for run in runs {
        let Some(lcn) = run.lcn else {
            return Err(unsupported("Sparse MFT data is not supported."));
        };
        if run.vcn < next_vcn
            || (contiguous && run.vcn != next_vcn)
            || run.clusters == 0
            || lcn
                .checked_add(run.clusters)
                .is_none_or(|end| end > geometry.clusters)
        {
            return Err(invalid(
                "MFT data runs are discontinuous or outside the volume.",
            ));
        }
        next_vcn = run
            .vcn
            .checked_add(run.clusters)
            .ok_or_else(|| invalid("MFT virtual cluster overflow."))?;
        physical_ranges.push((lcn, lcn + run.clusters));
    }
    physical_ranges.sort_unstable_by_key(|range| range.0);
    if physical_ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(invalid("MFT physical data runs overlap."));
    }
    next_vcn
        .checked_mul(geometry.cluster)
        .ok_or_else(|| invalid("MFT mapping length overflow."))
}

fn validate_runs(runs: &[Run], geometry: &Geometry, length: u64) -> io::Result<()> {
    if validate_mapping(runs, geometry, true)? < length {
        return Err(unsupported(
            "MFT extents do not cover initialized data; additional attribute-list extents may be required.",
        ));
    }
    Ok(())
}

/// Plan a logical read using only known extents. A missing extent is deferred
/// during bootstrap; callers never guess that adjacent physical clusters are
/// adjacent portions of the MFT.
fn mapped_ranges(
    runs: &[Run],
    geometry: &Geometry,
    offset: u64,
    length: usize,
) -> io::Result<Vec<(u64, usize)>> {
    let end = offset
        .checked_add(length as u64)
        .ok_or_else(|| invalid("MFT logical read overflow."))?;
    let mut position = offset;
    let mut ranges = Vec::new();
    for run in runs {
        let start = run
            .vcn
            .checked_mul(geometry.cluster)
            .ok_or_else(|| invalid("MFT logical extent overflow."))?;
        let stop = run
            .vcn
            .checked_add(run.clusters)
            .and_then(|end| end.checked_mul(geometry.cluster))
            .ok_or_else(|| invalid("MFT logical extent overflow."))?;
        if stop <= position {
            continue;
        }
        if start > position {
            break;
        }
        let count = (stop.min(end) - position) as usize;
        let physical = run
            .lcn
            .and_then(|lcn| lcn.checked_mul(geometry.cluster))
            .and_then(|physical_start| physical_start.checked_add(position - start))
            .ok_or_else(|| invalid("MFT physical read offset overflow."))?;
        if physical
            .checked_add(count as u64)
            .is_none_or(|stop| stop > geometry.volume_bytes)
        {
            return Err(invalid("MFT physical read exceeds volume bounds."));
        }
        ranges.push((physical, count));
        position += count as u64;
        if position == end {
            return Ok(ranges);
        }
    }
    Err(unsupported(
        "MFT extension record is not reachable through the known bootstrap extents.",
    ))
}

fn read_mapped(
    file: &mut File,
    runs: &[Run],
    geometry: &Geometry,
    buffer: &mut ReadBuffer,
    offset: u64,
    length: usize,
) -> io::Result<Vec<u8>> {
    let ranges = mapped_ranges(runs, geometry, offset, length)?;
    let mut output = Vec::with_capacity(length);
    for (physical, count) in ranges {
        let leading = physical as usize % geometry.sector;
        let aligned = physical - leading as u64;
        let count_aligned = (count + leading).next_multiple_of(geometry.sector);
        if count_aligned > CHUNK_BYTES {
            return Err(unsupported(
                "MFT bootstrap read exceeds its bounded buffer.",
            ));
        }
        let bytes = buffer.bytes(count_aligned);
        read_at(file, aligned, bytes, geometry.sector)?;
        output.extend_from_slice(&bytes[leading..leading + count]);
    }
    Ok(output)
}

fn resolve_mft_runs(
    file: &mut File,
    bootstrap: &mut Record,
    geometry: &Geometry,
    buffer: &mut ReadBuffer,
    length: u64,
    cancel: &CancelToken,
) -> io::Result<Vec<Run>> {
    let mut runs = bootstrap
        .data_runs
        .take()
        .ok_or_else(|| invalid("MFT record zero has no nonresident DATA runs."))?;
    runs.sort_unstable_by_key(|run| run.vcn);
    validate_mapping(&runs, geometry, false)?;
    if runs[0].vcn != 0 || runs[0].lcn != Some(geometry.mft_lcn) {
        return Err(invalid(
            "MFT DATA mapping disagrees with its volume bootstrap location.",
        ));
    }
    if !bootstrap.has_attribute_list {
        return Ok(runs);
    }
    let entries = if let Some(entries) = bootstrap.attribute_list.take() {
        entries
    } else if let Some(list_runs) = bootstrap.attribute_list_runs.as_mut() {
        let size = bootstrap
            .attribute_list_size
            .ok_or_else(|| unsupported("MFT attribute-list size is unavailable."))?;
        let initialized = bootstrap
            .attribute_list_initialized
            .ok_or_else(|| unsupported("MFT attribute-list initialized length is unavailable."))?;
        if size == 0 || size > MAX_ATTRIBUTE_LIST_BYTES as u64 || initialized != size {
            return Err(unsupported(
                "MFT attribute list is incomplete or exceeds the 1 MiB bootstrap limit.",
            ));
        }
        list_runs.sort_unstable_by_key(|run| run.vcn);
        validate_runs(list_runs, geometry, size)?;
        let bytes = read_mapped(file, list_runs, geometry, buffer, 0, size as usize)?;
        ntfs_records::attribute_list(&bytes)?
    } else {
        return Err(unsupported(
            "MFT attribute list cannot be resolved from record zero.",
        ));
    };
    let base_reference = u64::from(bootstrap.sequence) << 48;
    let mut pending = BTreeMap::<u64, Vec<(u16, u64)>>::new();
    let mut base_extents = Vec::new();
    for entry in entries
        .into_iter()
        .filter(|entry| entry.kind == 0x80 && entry.unnamed)
    {
        let key = (entry.attribute_id, entry.lowest_vcn);
        if entry.reference & REFERENCE_MASK == 0 {
            if entry.reference != base_reference || !bootstrap.data_extents.contains(&key) {
                return Err(invalid(
                    "MFT attribute-list base reference does not match record zero.",
                ));
            }
            if base_extents.contains(&key) {
                return Err(invalid("Duplicate MFT base attribute-list extent."));
            }
            base_extents.push(key);
        } else {
            let expected = pending.entry(entry.reference).or_default();
            if expected.contains(&key) {
                return Err(invalid("Duplicate MFT attribute-list extent."));
            }
            expected.push(key);
        }
    }
    if base_extents.len() != bootstrap.data_extents.len() {
        return Err(invalid("MFT attribute list omits a base DATA extent."));
    }
    if pending.len() > MAX_BOOTSTRAP_RECORDS {
        return Err(unsupported(
            "MFT needs more than 1024 bootstrap extension records.",
        ));
    }
    while !pending.is_empty() {
        if cancel.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "MFT bootstrap cancelled.",
            ));
        }
        let mut resolved = Vec::new();
        for (&reference, expected) in &pending {
            if cancel.is_cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "MFT bootstrap cancelled.",
                ));
            }
            let ordinal = reference & REFERENCE_MASK;
            let offset = ordinal
                .checked_mul(geometry.record as u64)
                .ok_or_else(|| invalid("MFT extension reference overflow."))?;
            if offset
                .checked_add(geometry.record as u64)
                .is_none_or(|end| end > length)
            {
                return Err(invalid(
                    "MFT extension reference exceeds initialized records.",
                ));
            }
            let bytes = match read_mapped(file, &runs, geometry, buffer, offset, geometry.record) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::Unsupported => continue,
                Err(error) => return Err(error),
            };
            let mut extension = ntfs_records::parse(&bytes, geometry.sector)?
                .ok_or_else(|| invalid("MFT extension record is no longer in use."))?;
            if extension.base_reference != base_reference
                || extension.sequence != (reference >> 48) as u16
                || extension.has_attribute_list
                || extension.data_extents.len() != expected.len()
                || expected
                    .iter()
                    .any(|key| !extension.data_extents.contains(key))
                || extension.data_size.is_some()
            {
                return Err(invalid(
                    "MFT extension sequence, base reference, or DATA attribute identity changed.",
                ));
            }
            runs.extend(
                extension
                    .data_runs
                    .take()
                    .ok_or_else(|| invalid("MFT extension contains no DATA runs."))?,
            );
            if runs.len() > MAX_BOOTSTRAP_RUNS {
                return Err(unsupported(
                    "MFT exceeds the bounded bootstrap extent limit.",
                ));
            }
            runs.sort_unstable_by_key(|run| run.vcn);
            validate_mapping(&runs, geometry, false)?;
            resolved.push(reference);
        }
        if resolved.is_empty() {
            return Err(unsupported(
                "MFT attribute-list extensions cannot be reached from the available bootstrap extents.",
            ));
        }
        for reference in resolved {
            pending.remove(&reference);
        }
    }
    Ok(runs)
}

/// Stream complete MFT slots from a local drive root. The caller owns record
/// retention; this reader uses one 8 MiB buffer and one file-record carry buffer.
/// `progress(read, total)` counts slots, including unused/deleted records.
pub(super) fn read(
    root: &Path,
    cancel: &CancelToken,
    mut on_record: impl FnMut(Record),
    mut progress: impl FnMut(u64, u64),
) -> io::Result<VolumeSummary> {
    let device = device_path(root)?;
    if cancel.is_cancelled() {
        return Ok(VolumeSummary {
            root_reference: 0,
            cancelled: true,
            records_read: 0,
        });
    }
    let mut file = OpenOptions::new()
        .read(true)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
        .custom_flags(FILE_FLAG_NO_BUFFERING.0)
        .open(device)?;
    let geometry = geometry(&file)?;
    check_bootstrap(&file, geometry.record)?;
    let mut buffer = ReadBuffer::new(geometry.sector);
    let bootstrap_bytes = geometry.record.next_multiple_of(geometry.sector);
    let bootstrap_offset = geometry
        .mft_lcn
        .checked_mul(geometry.cluster)
        .ok_or_else(|| invalid("MFT bootstrap offset overflow."))?;
    if bootstrap_offset
        .checked_add(bootstrap_bytes as u64)
        .is_none_or(|end| end > geometry.volume_bytes)
    {
        return Err(invalid("MFT bootstrap is outside the volume."));
    }
    // Read on-disk bytes rather than trusting the FSCTL buffer's fixup state.
    // The common parser verifies every update-sequence trailer before use.
    read_at(
        &mut file,
        bootstrap_offset,
        buffer.bytes(bootstrap_bytes),
        geometry.sector,
    )?;
    let mut bootstrap = ntfs_records::parse(
        &buffer.bytes(bootstrap_bytes)[..geometry.record],
        geometry.sector,
    )?
    .ok_or_else(|| invalid("MFT record zero is not in use."))?;
    if bootstrap.base_reference != 0 {
        return Err(invalid(
            "MFT record zero is an extension rather than a base record.",
        ));
    }
    let initialized = bootstrap
        .data_initialized
        .ok_or_else(|| invalid("MFT initialized data length is missing."))?;
    let data_size = bootstrap
        .data_size
        .ok_or_else(|| invalid("MFT data length is missing."))?;
    if initialized > data_size {
        return Err(invalid(
            "MFT initialized data exceeds the DATA stream length.",
        ));
    }
    let length = geometry.initialized.min(initialized).min(data_size);
    if length < geometry.record as u64 * 6
        || !length.is_multiple_of(geometry.record as u64)
        || !length.is_multiple_of(geometry.sector as u64)
    {
        return Err(invalid(
            "MFT initialized data does not contain complete aligned records.",
        ));
    }
    let runs = match resolve_mft_runs(
        &mut file,
        &mut bootstrap,
        &geometry,
        &mut buffer,
        length,
        cancel,
    ) {
        Ok(runs) => runs,
        Err(_) if cancel.is_cancelled() => {
            return Ok(VolumeSummary {
                root_reference: 0,
                cancelled: true,
                records_read: 0,
            });
        }
        Err(error) => return Err(error),
    };
    validate_runs(&runs, &geometry, length)?;
    let total = length / geometry.record as u64;
    let mut summary = VolumeSummary {
        root_reference: 0,
        cancelled: false,
        records_read: 0,
    };
    let mut consumed = 0u64;
    let mut carry = Vec::with_capacity(geometry.record);
    let mut last_update = Instant::now();
    progress(0, total);
    'runs: for run in runs {
        let run_bytes = run
            .clusters
            .checked_mul(geometry.cluster)
            .ok_or_else(|| invalid("MFT run length overflow."))?;
        let run_read = run_bytes.min(length - consumed);
        let physical = run
            .lcn
            .unwrap()
            .checked_mul(geometry.cluster)
            .ok_or_else(|| invalid("MFT run offset overflow."))?;
        let mut offset = 0u64;
        while offset < run_read {
            if cancel.is_cancelled() {
                summary.cancelled = true;
                break 'runs;
            }
            let count = (run_read - offset).min(CHUNK_BYTES as u64) as usize;
            let bytes = buffer.bytes(count);
            read_at(&mut file, physical + offset, bytes, geometry.sector)?;
            let mut cursor = 0;
            while cursor < bytes.len() {
                if cancel.is_cancelled() {
                    summary.cancelled = true;
                    break 'runs;
                }
                let needed = geometry.record - carry.len();
                let available = bytes.len() - cursor;
                let record = if carry.is_empty() && available >= geometry.record {
                    let start = cursor;
                    cursor += geometry.record;
                    &bytes[start..cursor]
                } else {
                    let copy = needed.min(available);
                    carry.extend_from_slice(&bytes[cursor..cursor + copy]);
                    cursor += copy;
                    if carry.len() < geometry.record {
                        continue;
                    }
                    carry.as_slice()
                };
                if let Some(mut record) = ntfs_records::parse(record, geometry.sector)? {
                    record.number = summary.records_read;
                    if record.number == 5 {
                        if !record.is_directory {
                            return Err(invalid("NTFS root record is not a directory."));
                        }
                        summary.root_reference = (u64::from(record.sequence) << 48) | 5;
                    }
                    on_record(record);
                }
                summary.records_read += 1;
                carry.clear();
            }
            offset += count as u64;
            consumed += count as u64;
            if last_update.elapsed() >= Duration::from_millis(100) {
                progress(summary.records_read, total);
                last_update = Instant::now();
            }
        }
        if consumed == length {
            break;
        }
    }
    summary.cancelled |= cancel.is_cancelled();
    if !summary.cancelled
        && (summary.records_read != total || !carry.is_empty() || summary.root_reference == 0)
    {
        return Err(invalid(
            "MFT stream ended without a complete root directory and record set.",
        ));
    }
    progress(summary.records_read, total);
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_paths_accept_only_drive_roots() {
        assert_eq!(device_path(Path::new(r"c:\")).unwrap(), r"\\.\C:");
        assert_eq!(device_path(Path::new(r"\\?\D:\")).unwrap(), r"\\.\D:");
        for path in [
            r"C:",
            r"C:\Users",
            r"\\server\share\",
            r"\\.\C:",
            r"C:\..\",
            "C:/folder",
            "/",
            "",
        ] {
            assert_eq!(
                device_path(Path::new(path)).unwrap_err().kind(),
                io::ErrorKind::Unsupported
            );
        }
    }

    fn geometry_fixture() -> Geometry {
        Geometry {
            sector: 512,
            cluster: 4096,
            record: 1024,
            initialized: 8192,
            mft_lcn: 1,
            clusters: 100,
            volume_bytes: 409600,
        }
    }

    #[test]
    fn run_validation_rejects_gaps_sparse_overlap_and_out_of_volume() {
        let geometry = geometry_fixture();
        for runs in [
            vec![],
            vec![Run {
                vcn: 1,
                lcn: Some(1),
                clusters: 2,
            }],
            vec![Run {
                vcn: 0,
                lcn: None,
                clusters: 2,
            }],
            vec![Run {
                vcn: 0,
                lcn: Some(99),
                clusters: 2,
            }],
            vec![
                Run {
                    vcn: 0,
                    lcn: Some(1),
                    clusters: 2,
                },
                Run {
                    vcn: 2,
                    lcn: Some(2),
                    clusters: 1,
                },
            ],
            vec![Run {
                vcn: 0,
                lcn: Some(1),
                clusters: 1,
            }],
        ] {
            assert!(validate_runs(&runs, &geometry, 8192).is_err());
        }
        assert!(
            validate_runs(
                &[
                    Run {
                        vcn: 0,
                        lcn: Some(1),
                        clusters: 1
                    },
                    Run {
                        vcn: 1,
                        lcn: Some(20),
                        clusters: 1
                    }
                ],
                &geometry,
                8192
            )
            .is_ok()
        );
    }

    #[test]
    fn read_buffer_is_aligned_for_supported_sectors() {
        for sector in [512, 4096] {
            let mut buffer = ReadBuffer::new(sector);
            let bytes = buffer.bytes(CHUNK_BYTES);
            assert_eq!(bytes.as_ptr() as usize % sector, 0);
            assert_eq!(bytes.as_ptr() as usize % 4096, 0);
            assert_eq!(bytes.len(), CHUNK_BYTES);
        }
    }

    #[test]
    fn logical_reads_cross_fragment_boundaries_without_guessing_physical_contiguity() {
        let geometry = geometry_fixture();
        let runs = [
            Run {
                vcn: 0,
                lcn: Some(10),
                clusters: 1,
            },
            Run {
                vcn: 1,
                lcn: Some(30),
                clusters: 1,
            },
        ];
        assert_eq!(
            mapped_ranges(&runs, &geometry, 3072, 2048).unwrap(),
            vec![(44032, 1024), (122880, 1024)]
        );
        assert_eq!(
            mapped_ranges(&runs[..1], &geometry, 3072, 2048)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        let gapped = [
            Run {
                vcn: 0,
                lcn: Some(10),
                clusters: 1,
            },
            Run {
                vcn: 2,
                lcn: Some(30),
                clusters: 1,
            },
        ];
        assert!(validate_mapping(&gapped, &geometry, false).is_ok());
        assert!(validate_runs(&gapped, &geometry, 12288).is_err());
        assert_eq!(
            mapped_ranges(&gapped, &geometry, 8192, 1024).unwrap(),
            vec![(122880, 1024)]
        );
    }

    fn raw_data_record(
        lowest: u64,
        lcn: u8,
        base: u64,
        sequence: u16,
        attribute_id: u16,
    ) -> Vec<u8> {
        let mut bytes = vec![0u8; 1024];
        bytes[..4].copy_from_slice(b"FILE");
        bytes[4..6].copy_from_slice(&48u16.to_le_bytes());
        bytes[6..8].copy_from_slice(&3u16.to_le_bytes());
        bytes[16..18].copy_from_slice(&sequence.to_le_bytes());
        bytes[20..22].copy_from_slice(&64u16.to_le_bytes());
        bytes[22..24].copy_from_slice(&1u16.to_le_bytes());
        bytes[24..28].copy_from_slice(&144u32.to_le_bytes());
        bytes[28..32].copy_from_slice(&1024u32.to_le_bytes());
        bytes[32..40].copy_from_slice(&base.to_le_bytes());
        bytes[48..50].copy_from_slice(&0xaaaau16.to_le_bytes());
        bytes[510..512].copy_from_slice(&0xaaaau16.to_le_bytes());
        bytes[1022..1024].copy_from_slice(&0xaaaau16.to_le_bytes());
        let attr = &mut bytes[64..136];
        attr[..4].copy_from_slice(&0x80u32.to_le_bytes());
        attr[4..8].copy_from_slice(&72u32.to_le_bytes());
        attr[8] = 1;
        attr[14..16].copy_from_slice(&attribute_id.to_le_bytes());
        attr[16..24].copy_from_slice(&lowest.to_le_bytes());
        attr[24..32].copy_from_slice(&lowest.to_le_bytes());
        attr[32..34].copy_from_slice(&64u16.to_le_bytes());
        if lowest == 0 {
            attr[40..48].copy_from_slice(&8192u64.to_le_bytes());
            attr[48..56].copy_from_slice(&8192u64.to_le_bytes());
            attr[56..64].copy_from_slice(&8192u64.to_le_bytes());
        }
        attr[64..68].copy_from_slice(&[0x11, 1, lcn, 0]);
        bytes[136..140].copy_from_slice(&u32::MAX.to_le_bytes());
        bytes
    }

    fn continuation_fixture(extension_sequence: u16) -> (File, Record, Geometry) {
        use std::io::Write;
        let mut file = tempfile::tempfile().unwrap();
        let geometry = geometry_fixture();
        file.set_len(geometry.volume_bytes).unwrap();
        let base_bytes = raw_data_record(0, 1, 0, 7, 3);
        file.seek(SeekFrom::Start(4096)).unwrap();
        file.write_all(&base_bytes).unwrap();
        file.seek(SeekFrom::Start(4096 + 1024)).unwrap();
        file.write_all(&raw_data_record(1, 20, 7 << 48, extension_sequence, 4))
            .unwrap();
        let mut bootstrap = ntfs_records::parse(&base_bytes, 512).unwrap().unwrap();
        bootstrap.has_attribute_list = true;
        bootstrap.attribute_list = Some(vec![
            ntfs_records::AttributeEntry {
                kind: 0x80,
                lowest_vcn: 0,
                reference: 7 << 48,
                attribute_id: 3,
                unnamed: true,
            },
            ntfs_records::AttributeEntry {
                kind: 0x80,
                lowest_vcn: 1,
                reference: (9 << 48) | 1,
                attribute_id: 4,
                unnamed: true,
            },
        ]);
        (file, bootstrap, geometry)
    }

    #[test]
    fn resident_attribute_list_resolves_checked_raw_extension_record() {
        let (mut file, mut bootstrap, geometry) = continuation_fixture(9);
        let mut buffer = ReadBuffer::new(512);
        let runs = resolve_mft_runs(
            &mut file,
            &mut bootstrap,
            &geometry,
            &mut buffer,
            8192,
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(
            (runs[1].vcn, runs[1].lcn, runs[1].clusters),
            (1, Some(20), 1)
        );
        assert!(validate_runs(&runs, &geometry, 8192).is_ok());
    }

    #[test]
    fn nonresident_attribute_list_reads_only_its_declared_bytes() {
        use std::io::Write;
        let (mut file, mut bootstrap, geometry) = continuation_fixture(9);
        let mut bytes = vec![0u8; 64];
        for (entry, (lowest, reference, attribute_id)) in bytes
            .as_chunks_mut::<32>()
            .0
            .iter_mut()
            .zip([(0u64, 7u64 << 48, 3u16), (1, (9 << 48) | 1, 4)])
        {
            entry[..4].copy_from_slice(&0x80u32.to_le_bytes());
            entry[4..6].copy_from_slice(&32u16.to_le_bytes());
            entry[8..16].copy_from_slice(&lowest.to_le_bytes());
            entry[16..24].copy_from_slice(&reference.to_le_bytes());
            entry[24..26].copy_from_slice(&attribute_id.to_le_bytes());
        }
        file.seek(SeekFrom::Start(30 * 4096)).unwrap();
        file.write_all(&bytes).unwrap();
        bootstrap.attribute_list = None;
        bootstrap.attribute_list_runs = Some(vec![Run {
            vcn: 0,
            lcn: Some(30),
            clusters: 1,
        }]);
        bootstrap.attribute_list_size = Some(64);
        bootstrap.attribute_list_initialized = Some(64);
        let runs = resolve_mft_runs(
            &mut file,
            &mut bootstrap,
            &geometry,
            &mut ReadBuffer::new(512),
            8192,
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(runs.len(), 2);
    }

    #[test]
    fn stale_or_unreachable_extension_falls_back_without_using_unverified_runs() {
        let (mut file, mut bootstrap, geometry) = continuation_fixture(8);
        let error = resolve_mft_runs(
            &mut file,
            &mut bootstrap,
            &geometry,
            &mut ReadBuffer::new(512),
            8192,
            &CancelToken::new(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let (mut file, mut bootstrap, geometry) = continuation_fixture(9);
        bootstrap.attribute_list.as_mut().unwrap()[1].reference = (9 << 48) | 4;
        let error = resolve_mft_runs(
            &mut file,
            &mut bootstrap,
            &geometry,
            &mut ReadBuffer::new(512),
            8192,
            &CancelToken::new(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }
}
