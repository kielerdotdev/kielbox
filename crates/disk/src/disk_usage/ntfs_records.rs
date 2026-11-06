//! NTFS 3.x FILE records. Layout and mapping-pair interpretation follow
//! Microsoft's FILE_RECORD_SEGMENT_HEADER, ATTRIBUTE_RECORD_HEADER and
//! MULTI_SECTOR_HEADER developer notes. No filesystem access occurs here.
use std::{ffi::OsString, io};

#[derive(Debug)]
pub(super) struct Record {
    pub number: u64,
    pub sequence: u16,
    pub base_reference: u64,
    pub is_directory: bool,
    pub attributes: u32,
    pub names: Vec<Name>,
    pub data_size: Option<u64>,
    pub data_runs: Option<Vec<Run>>,
    pub data_initialized: Option<u64>,
    pub has_attribute_list: bool,
    pub attribute_list: Option<Vec<AttributeEntry>>,
    pub attribute_list_runs: Option<Vec<Run>>,
    pub attribute_list_size: Option<u64>,
    pub attribute_list_initialized: Option<u64>,
    /// Attribute instance and starting VCN for each unnamed DATA attribute.
    pub data_extents: Vec<(u16, u64)>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct AttributeEntry {
    pub kind: u32,
    pub lowest_vcn: u64,
    pub reference: u64,
    pub attribute_id: u16,
    pub unnamed: bool,
}

/// Decode one complete ATTRIBUTE_LIST value. The reader imposes the same cap
/// before allocating nonresident list data; retain it here for direct callers.
pub(super) fn attribute_list(bytes: &[u8]) -> io::Result<Vec<AttributeEntry>> {
    if bytes.len() > 1024 * 1024 {
        return Err(invalid("NTFS attribute list exceeds 1 MiB"));
    }
    let mut entries = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let remaining = &bytes[offset..];
        if remaining.len() < 8 && remaining.iter().all(|&byte| byte == 0) {
            break;
        }
        let kind = u32_at(remaining, 0)?;
        if kind == u32::MAX {
            if remaining[4..].iter().any(|&byte| byte != 0) {
                return Err(invalid("Data follows NTFS attribute list terminator"));
            }
            break;
        }
        let length = u16_at(remaining, 4)? as usize;
        if kind == 0 || length < 26 || !length.is_multiple_of(8) {
            return Err(invalid("Invalid NTFS attribute list entry length"));
        }
        let entry = slice(remaining, 0, length)?;
        let name_length = entry[6] as usize * 2;
        if name_length != 0 {
            let name_offset = entry[7] as usize;
            if name_offset < 26 || !name_offset.is_multiple_of(2) {
                return Err(invalid("Invalid NTFS attribute list name offset"));
            }
            slice(entry, name_offset, name_length)?;
        }
        entries.push(AttributeEntry {
            kind,
            lowest_vcn: u64_at(entry, 8)?,
            reference: u64_at(entry, 16)?,
            attribute_id: u16_at(entry, 24)?,
            unnamed: name_length == 0,
        });
        offset += length;
    }
    Ok(entries)
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Name {
    /// Full NTFS reference: low 48-bit record number, high 16-bit sequence.
    pub parent_reference: u64,
    pub name: OsString,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Run {
    pub vcn: u64,
    /// None denotes an unallocated sparse extent.
    pub lcn: Option<u64>,
    pub clusters: u64,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn slice(bytes: &[u8], offset: usize, length: usize) -> io::Result<&[u8]> {
    bytes
        .get(
            offset
                ..offset
                    .checked_add(length)
                    .ok_or_else(|| invalid("NTFS offset overflow"))?,
        )
        .ok_or_else(|| invalid("Truncated NTFS record"))
}
fn u16_at(bytes: &[u8], offset: usize) -> io::Result<u16> {
    Ok(u16::from_le_bytes(
        slice(bytes, offset, 2)?.try_into().unwrap(),
    ))
}
fn u32_at(bytes: &[u8], offset: usize) -> io::Result<u32> {
    Ok(u32::from_le_bytes(
        slice(bytes, offset, 4)?.try_into().unwrap(),
    ))
}
fn u64_at(bytes: &[u8], offset: usize) -> io::Result<u64> {
    Ok(u64::from_le_bytes(
        slice(bytes, offset, 8)?.try_into().unwrap(),
    ))
}

fn fixed_record(bytes: &[u8], bytes_per_sector: usize) -> io::Result<Vec<u8>> {
    if !(512..=65536).contains(&bytes_per_sector)
        || !bytes_per_sector.is_power_of_two()
        || bytes.len() < 512
        || !bytes.len().is_multiple_of(512)
    {
        return Err(invalid("Invalid NTFS sector or record size"));
    }
    let offset = u16_at(bytes, 4)? as usize;
    let count = u16_at(bytes, 6)? as usize;
    if count < 2 || !bytes.len().is_multiple_of(count - 1) {
        return Err(invalid("Invalid NTFS update sequence count"));
    }
    let stride = bytes.len() / (count - 1);
    // The NTFS update sequence stride is commonly 512 even when the physical
    // sector is larger. Also accept the volume's logical sector stride.
    if stride != 512 && stride != bytes_per_sector {
        return Err(invalid("Invalid NTFS update sequence stride"));
    }
    let usa = slice(bytes, offset, count * 2)?;
    if offset < 48 || !offset.is_multiple_of(2) || offset + usa.len() > stride - 2 {
        return Err(invalid("Invalid NTFS update sequence location"));
    }
    let marker = &usa[..2];
    let mut fixed = bytes.to_vec();
    for sector in 1..count {
        let trailer = sector * stride - 2;
        if slice(bytes, trailer, 2)? != marker {
            return Err(invalid("NTFS update sequence mismatch (torn record)"));
        }
        fixed[trailer..trailer + 2].copy_from_slice(&usa[sector * 2..sector * 2 + 2]);
    }
    Ok(fixed)
}

pub(super) fn parse(bytes: &[u8], bytes_per_sector: usize) -> io::Result<Option<Record>> {
    if !bytes.is_empty() && bytes.iter().all(|byte| *byte == 0) {
        return Ok(None);
    }
    if slice(bytes, 0, 4)? != b"FILE" {
        return Err(invalid("Invalid NTFS FILE signature"));
    }
    let fixed = fixed_record(bytes, bytes_per_sector)?;
    let bytes = fixed.as_slice();
    let flags = u16_at(bytes, 22)?;
    if flags & 1 == 0 {
        return Ok(None);
    }
    let used = u32_at(bytes, 24)? as usize;
    let allocated = u32_at(bytes, 28)? as usize;
    let mut offset = u16_at(bytes, 20)? as usize;
    let usa_end = u16_at(bytes, 4)? as usize + u16_at(bytes, 6)? as usize * 2;
    if allocated != bytes.len()
        || used > allocated
        || offset < usa_end
        || offset < 48
        || !offset.is_multiple_of(8)
        || offset.checked_add(4).is_none_or(|end| end > used)
    {
        return Err(invalid("Invalid NTFS attribute boundary"));
    }
    let mut result = Record {
        number: u32_at(bytes, 44)? as u64,
        sequence: u16_at(bytes, 16)?,
        base_reference: u64_at(bytes, 32)?,
        is_directory: flags & 2 != 0,
        attributes: 0,
        names: Vec::new(),
        data_size: None,
        data_runs: None,
        data_initialized: None,
        has_attribute_list: false,
        attribute_list: None,
        attribute_list_runs: None,
        attribute_list_size: None,
        attribute_list_initialized: None,
        data_extents: Vec::new(),
    };
    let bytes = &bytes[..used];
    loop {
        let kind = u32_at(bytes, offset)?;
        if kind == u32::MAX {
            break;
        }
        let length = u32_at(bytes, offset + 4)? as usize;
        if kind == 0 || length < 24 || !length.is_multiple_of(8) {
            return Err(invalid("Invalid NTFS attribute length"));
        }
        let attr = slice(bytes, offset, length)?;
        let nonresident = attr[8];
        let header_length = match nonresident {
            0 => 24,
            1 => 64,
            _ => return Err(invalid("Invalid NTFS attribute form")),
        };
        slice(attr, 0, header_length)?;
        let name_length = attr[9] as usize * 2;
        let mut content_start = header_length;
        if name_length != 0 {
            let name_offset = u16_at(attr, 10)? as usize;
            if name_offset < header_length || !name_offset.is_multiple_of(2) {
                return Err(invalid("Invalid NTFS attribute name"));
            }
            slice(attr, name_offset, name_length)?;
            content_start = name_offset + name_length;
        }
        if kind == 0x20 {
            result.has_attribute_list = true;
        }
        if nonresident == 0 {
            let value_length = u32_at(attr, 16)? as usize;
            let value_offset = u16_at(attr, 20)? as usize;
            if value_offset < content_start {
                return Err(invalid("Invalid resident attribute value"));
            }
            let value = slice(attr, value_offset, value_length)?;
            match kind {
                0x10 => result.attributes = u32_at(value, 32)?,
                0x20 => {
                    if name_length != 0
                        || result.attribute_list.is_some()
                        || result.attribute_list_runs.is_some()
                    {
                        return Err(invalid("Duplicate or named NTFS attribute list"));
                    }
                    result.attribute_list = Some(attribute_list(value)?);
                }
                0x30 => {
                    slice(value, 0, 66)?;
                    let chars = value[64] as usize;
                    let namespace = value[65];
                    if namespace > 3 {
                        return Err(invalid("Invalid NTFS file name namespace"));
                    }
                    let encoded = slice(value, 66, chars * 2)?;
                    if namespace != 2 {
                        let name = Name {
                            parent_reference: u64_at(value, 0)?,
                            name: utf16_name(encoded)?,
                        };
                        if !result.names.contains(&name) {
                            result.names.push(name);
                        }
                    }
                }
                0x80 if name_length == 0 => {
                    if result.data_size.replace(value_length as u64).is_some()
                        || result.data_runs.is_some()
                    {
                        return Err(invalid("Duplicate resident NTFS data stream"));
                    }
                    result.data_initialized = Some(value_length as u64);
                    result.data_extents.push((u16_at(attr, 14)?, 0));
                }
                _ => {}
            }
        } else {
            if kind == 0x10 || kind == 0x30 {
                return Err(invalid("Nonresident NTFS file metadata"));
            }
            let run_offset = u16_at(attr, 32)? as usize;
            if run_offset < content_start || run_offset >= attr.len() {
                return Err(invalid("Invalid NTFS mapping pairs offset"));
            }
            if (kind == 0x80 && name_length == 0) || kind == 0x20 {
                let lowest = u64_at(attr, 16)?;
                let highest = u64_at(attr, 24)?;
                let runs = runlist(&attr[run_offset..], lowest, highest)?;
                let (size, initialized) = if lowest == 0 {
                    let size = u64_at(attr, 48)?;
                    let initialized = u64_at(attr, 56)?;
                    if initialized > size {
                        return Err(invalid("Invalid initialized NTFS data size"));
                    }
                    (Some(size), Some(initialized))
                } else {
                    (None, None)
                };
                if kind == 0x20 {
                    if name_length != 0 || result.attribute_list.is_some() {
                        return Err(invalid("Mixed resident or named NTFS attribute list"));
                    }
                    if lowest == 0 && result.attribute_list_size.is_some() {
                        return Err(invalid("Duplicate NTFS attribute list size"));
                    }
                    if size.is_some() {
                        result.attribute_list_size = size;
                        result.attribute_list_initialized = initialized;
                    }
                    result
                        .attribute_list_runs
                        .get_or_insert_with(Vec::new)
                        .extend(runs);
                } else {
                    if lowest == 0 && result.data_size.is_some() {
                        return Err(invalid("Duplicate NTFS data stream size"));
                    }
                    if size.is_some() {
                        result.data_size = size;
                        result.data_initialized = initialized;
                    }
                    result.data_runs.get_or_insert_with(Vec::new).extend(runs);
                    result.data_extents.push((u16_at(attr, 14)?, lowest));
                }
            }
        }
        offset += length;
    }
    Ok(Some(result))
}

fn utf16_name(bytes: &[u8]) -> io::Result<OsString> {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
        .collect();
    if units.is_empty()
        || units
            .iter()
            .any(|&unit| unit == 0 || unit == b'/' as u16 || unit == b'\\' as u16)
        || units == [b'.' as u16, b'.' as u16]
    {
        return Err(invalid("Invalid NTFS file name"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        Ok(OsString::from_wide(&units))
    }
    #[cfg(not(windows))]
    {
        Ok(OsString::from(String::from_utf16_lossy(&units)))
    }
}

fn runlist(bytes: &[u8], first_vcn: u64, last_vcn: u64) -> io::Result<Vec<Run>> {
    let mut cursor = 0;
    let mut vcn = first_vcn;
    let mut lcn = 0i128;
    let mut runs = Vec::new();
    loop {
        let header = *bytes
            .get(cursor)
            .ok_or_else(|| invalid("Unterminated NTFS mapping pairs"))?;
        cursor += 1;
        if header == 0 {
            break;
        }
        let length_bytes = (header & 15) as usize;
        let offset_bytes = (header >> 4) as usize;
        if length_bytes == 0 || length_bytes > 8 || offset_bytes > 8 {
            return Err(invalid("Invalid NTFS mapping pair width"));
        }
        let mut length = [0u8; 8];
        length[..length_bytes].copy_from_slice(slice(bytes, cursor, length_bytes)?);
        cursor += length_bytes;
        let clusters = u64::from_le_bytes(length);
        if clusters == 0 {
            return Err(invalid("Empty NTFS mapping pair"));
        }
        let physical = if offset_bytes == 0 {
            None
        } else {
            let encoded = slice(bytes, cursor, offset_bytes)?;
            cursor += offset_bytes;
            let mut delta = [if encoded[offset_bytes - 1] & 0x80 != 0 {
                255
            } else {
                0
            }; 8];
            delta[..offset_bytes].copy_from_slice(encoded);
            lcn += i64::from_le_bytes(delta) as i128;
            let lcn = u64::try_from(lcn).map_err(|_| invalid("Invalid NTFS physical cluster"))?;
            lcn.checked_add(clusters)
                .ok_or_else(|| invalid("NTFS physical extent overflow"))?;
            Some(lcn)
        };
        runs.push(Run {
            vcn,
            lcn: physical,
            clusters,
        });
        vcn = vcn
            .checked_add(clusters)
            .ok_or_else(|| invalid("NTFS virtual extent overflow"))?;
    }
    if !((runs.is_empty() && first_vcn == 0 && last_vcn == u64::MAX)
        || last_vcn.checked_add(1) == Some(vcn))
    {
        return Err(invalid(
            "NTFS mapping pairs do not cover the declared VCN range",
        ));
    }
    Ok(runs)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn put16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
    fn put32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn put64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    fn resident(kind: u32, name: &str, value: &[u8]) -> Vec<u8> {
        let name: Vec<u16> = name.encode_utf16().collect();
        let offset = (24 + name.len() * 2 + 7) & !7;
        let length = (offset + value.len() + 7) & !7;
        let mut attr = vec![0; length];
        put32(&mut attr, 0, kind);
        put32(&mut attr, 4, length as u32);
        attr[9] = name.len() as u8;
        put16(&mut attr, 10, 24);
        put32(&mut attr, 16, value.len() as u32);
        put16(&mut attr, 20, offset as u16);
        for (index, unit) in name.iter().enumerate() {
            put16(&mut attr, 24 + index * 2, *unit);
        }
        attr[offset..offset + value.len()].copy_from_slice(value);
        attr
    }
    fn file_name(parent: u64, name: &str, namespace: u8) -> Vec<u8> {
        let name: Vec<u16> = name.encode_utf16().collect();
        let mut value = vec![0; 66 + name.len() * 2];
        put64(&mut value, 0, parent);
        value[64] = name.len() as u8;
        value[65] = namespace;
        for (index, unit) in name.iter().enumerate() {
            put16(&mut value, 66 + index * 2, *unit);
        }
        resident(0x30, "", &value)
    }
    fn nonresident(runs: &[u8], first: u64, last: u64, size: u64) -> Vec<u8> {
        let length = (64 + runs.len() + 7) & !7;
        let mut attr = vec![0; length];
        put32(&mut attr, 0, 0x80);
        put32(&mut attr, 4, length as u32);
        attr[8] = 1;
        put64(&mut attr, 16, first);
        put64(&mut attr, 24, last);
        put16(&mut attr, 32, 64);
        put64(&mut attr, 40, size);
        put64(&mut attr, 48, size);
        put64(&mut attr, 56, size);
        attr[64..64 + runs.len()].copy_from_slice(runs);
        attr
    }
    fn record(attrs: &[Vec<u8>], flags: u16, base: u64) -> Vec<u8> {
        let mut record = vec![0; 1024];
        record[..4].copy_from_slice(b"FILE");
        put16(&mut record, 4, 48);
        put16(&mut record, 6, 3);
        put16(&mut record, 16, 7);
        put16(&mut record, 20, 56);
        put16(&mut record, 22, flags);
        put32(&mut record, 28, 1024);
        put64(&mut record, 32, base);
        put32(&mut record, 44, 42);
        let mut position = 56;
        for attr in attrs {
            record[position..position + attr.len()].copy_from_slice(attr);
            position += attr.len();
        }
        put32(&mut record, position, u32::MAX);
        put32(&mut record, 24, (position + 4) as u32);
        put16(&mut record, 48, 0xabcdu16);
        for sector in 1..3 {
            let saved = u16_at(&record, sector * 512 - 2).unwrap();
            put16(&mut record, 48 + sector * 2, saved);
            put16(&mut record, sector * 512 - 2, 0xabcd);
        }
        record
    }

    #[test]
    fn resident_sizes_exclude_ads_and_names_preserve_unicode_without_dos_duplicates() {
        let mut standard = vec![0; 48];
        put32(&mut standard, 32, 0x4025);
        let name = file_name(5 | (3 << 48), "動画🦀.mkv", 1);
        let bytes = record(
            &[
                resident(0x10, "", &standard),
                name.clone(),
                name,
                file_name(5, "VIDEO~1.MKV", 2),
                resident(0x80, "", &[0; 13]),
                resident(0x80, "Zone.Identifier", &[0; 100]),
            ],
            1,
            0,
        );
        let parsed = parse(&bytes, 512).unwrap().unwrap();
        assert_eq!(
            (parsed.number, parsed.sequence, parsed.attributes),
            (42, 7, 0x4025)
        );
        assert!(!parsed.is_directory);
        assert_eq!(
            (parsed.data_size, parsed.data_initialized),
            (Some(13), Some(13))
        );
        assert_eq!(parsed.names.len(), 1);
        assert_eq!(parsed.names[0].parent_reference, 5 | (3 << 48));
        assert_eq!(parsed.names[0].name, "動画🦀.mkv");
    }

    #[test]
    fn mapping_pairs_support_negative_deltas_and_sparse_extents() {
        let bytes = record(
            &[nonresident(
                &[0x11, 2, 100, 0x01, 3, 0x11, 1, 0xfb, 0],
                0,
                5,
                24576,
            )],
            1,
            0,
        );
        let parsed = parse(&bytes, 512).unwrap().unwrap();
        assert_eq!(parsed.data_size, Some(24576));
        assert_eq!(
            parsed.data_runs.unwrap(),
            [
                Run {
                    vcn: 0,
                    lcn: Some(100),
                    clusters: 2
                },
                Run {
                    vcn: 2,
                    lcn: None,
                    clusters: 3
                },
                Run {
                    vcn: 5,
                    lcn: Some(95),
                    clusters: 1
                },
            ]
        );
    }

    #[test]
    fn deleted_records_skip_and_extension_records_preserve_base_reference() {
        assert!(parse(&record(&[], 0, 0), 512).unwrap().is_none());
        let base = 20 | (7 << 48);
        let bytes = record(
            &[
                resident(0x20, "", &[]),
                nonresident(&[0x11, 2, 10, 0], 3, 4, 0),
            ],
            3,
            base,
        );
        let parsed = parse(&bytes, 512).unwrap().unwrap();
        assert_eq!(parsed.base_reference, base);
        assert!(parsed.is_directory);
        assert!(parsed.has_attribute_list);
        assert_eq!(parsed.data_size, None);
        assert_eq!(parsed.data_runs.unwrap()[0].vcn, 3);
        assert!(parse(&[0; 1024], 512).unwrap().is_none());
    }

    #[test]
    fn fixups_restore_attribute_content_and_reject_torn_sectors() {
        let mut value = vec![3; 600];
        value[430] = 17;
        value[431] = 29;
        let bytes = record(&[resident(0x80, "", &value)], 1, 0);
        assert_eq!(&fixed_record(&bytes, 512).unwrap()[510..512], &[17, 29]);
        assert_eq!(parse(&bytes, 4096).unwrap().unwrap().data_size, Some(600));
        let mut torn = bytes.clone();
        torn[1022] ^= 1;
        assert!(parse(&torn, 512).is_err());
        let mut bad_count = bytes;
        put16(&mut bad_count, 6, 4);
        assert!(parse(&bad_count, 512).is_err());
    }

    #[test]
    fn malformed_ranges_and_mapping_pairs_return_errors() {
        let valid = record(&[resident(0x80, "", &[0; 4])], 1, 0);
        for length in 0..valid.len() {
            assert!(parse(&valid[..length], 512).is_err());
        }
        for (offset, value) in [(60, 0), (60, 0xfffffff8), (24, 2048), (28, 512)] {
            let mut malformed = valid.clone();
            put32(&mut malformed, offset, value);
            assert!(parse(&malformed, 512).is_err());
        }
        for runs in [
            &[0x10, 1, 0][..],
            &[0x11, 1, 0xff, 0],
            &[0x11, 0, 1, 0],
            &[0x91, 1, 0],
            &[0x11, 1],
        ] {
            assert!(runlist(runs, 0, 0).is_err());
        }
        assert!(runlist(&[0x11, 1, 1, 0], 0, 5).is_err());
    }

    #[test]
    fn arbitrary_single_byte_mutations_never_panic() {
        let valid = record(
            &[file_name(5, "file.bin", 1), resident(0x80, "", &[0; 16])],
            1,
            0,
        );
        for offset in 0..valid.len() {
            let mut changed = valid.clone();
            changed[offset] ^= 0xff;
            assert!(
                std::panic::catch_unwind(|| parse(&changed, 512)).is_ok(),
                "offset {offset}"
            );
        }
    }

    fn list_entry(
        kind: u32,
        lowest_vcn: u64,
        reference: u64,
        attribute_id: u16,
        name: &str,
    ) -> Vec<u8> {
        let name: Vec<u16> = name.encode_utf16().collect();
        let length = (26 + name.len() * 2 + 7) & !7;
        let mut entry = vec![0; length];
        put32(&mut entry, 0, kind);
        put16(&mut entry, 4, length as u16);
        entry[6] = name.len() as u8;
        entry[7] = 26;
        put64(&mut entry, 8, lowest_vcn);
        put64(&mut entry, 16, reference);
        put16(&mut entry, 24, attribute_id);
        for (index, unit) in name.iter().enumerate() {
            put16(&mut entry, 26 + index * 2, *unit);
        }
        entry
    }

    #[test]
    fn resident_attribute_list_retains_references_instances_and_named_stream_distinction() {
        let mut value = list_entry(0x80, 0, 7 << 48, 3, "");
        value.extend(list_entry(0x80, 9, (9 << 48) | 42, 4, ""));
        value.extend(list_entry(0x80, 0, 7 << 48, 5, "名"));
        let mut data = resident(0x80, "", &[0; 13]);
        put16(&mut data, 14, 3);
        let parsed = parse(&record(&[resident(0x20, "", &value), data], 1, 0), 512)
            .unwrap()
            .unwrap();
        assert!(parsed.has_attribute_list);
        let entries = parsed.attribute_list.unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries[1],
            AttributeEntry {
                kind: 0x80,
                lowest_vcn: 9,
                reference: (9 << 48) | 42,
                attribute_id: 4,
                unnamed: true
            }
        );
        assert!(!entries[2].unnamed);
        assert_eq!(parsed.data_extents, [(3, 0)]);
        assert!(parsed.attribute_list_runs.is_none());
    }

    #[test]
    fn nonresident_attribute_list_has_separate_mapping_and_sizes() {
        let mut list = nonresident(&[0x11, 1, 20, 0], 0, 0, 64);
        put32(&mut list, 0, 0x20);
        let mut data = nonresident(&[0x11, 1, 30, 0], 9, 9, 0);
        put16(&mut data, 14, 18);
        let parsed = parse(&record(&[list.clone(), data], 1, 0), 512)
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                parsed.attribute_list_size,
                parsed.attribute_list_initialized
            ),
            (Some(64), Some(64))
        );
        assert_eq!(
            parsed.attribute_list_runs.unwrap(),
            [Run {
                vcn: 0,
                lcn: Some(20),
                clusters: 1
            }]
        );
        assert_eq!(
            parsed.data_runs.unwrap(),
            [Run {
                vcn: 9,
                lcn: Some(30),
                clusters: 1
            }]
        );
        assert_eq!(parsed.data_extents, [(18, 9)]);
        assert!(parse(&record(&[list.clone(), list], 1, 0), 512).is_err());
    }

    #[test]
    fn malformed_attribute_list_entries_are_bounded_and_rejected() {
        let valid = list_entry(0x80, 0, 7 << 48, 3, "");
        for length in 1..valid.len() {
            assert!(attribute_list(&valid[..length]).is_err());
        }
        for length in [0, 24, 26, 31, 40] {
            let mut bad = valid.clone();
            put16(&mut bad, 4, length);
            assert!(attribute_list(&bad).is_err());
        }
        let mut bad_name = valid.clone();
        bad_name[6] = 4;
        bad_name[7] = 26;
        assert!(attribute_list(&bad_name).is_err());
        bad_name[6] = 1;
        bad_name[7] = 25;
        assert!(attribute_list(&bad_name).is_err());
        assert!(attribute_list(&vec![0; 1024 * 1024 + 1]).is_err());
        for offset in 0..valid.len() {
            let mut mutated = valid.clone();
            mutated[offset] ^= 255;
            assert!(std::panic::catch_unwind(|| attribute_list(&mutated)).is_ok());
        }
    }
}
