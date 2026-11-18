//! ZIP bytes for daily archives (D53). Writing goes through the `zip` crate
//! with deflate at a fixed level, a fixed timestamp and fixed permissions, so
//! the same entries always give the same bytes. Reading is a small
//! central-directory parser so an archived run opens with range reads of
//! just the tail, the central directory and its own folder
//! (`BlobStore::get_range`), never the whole file.
//!
//! Archives are untrusted when imported, so the reader accepts only what the
//! writer produces and refuses everything else explicitly:
//! - no ZIP64 (the writer refuses to need it: at most 65,535 entries and
//!   every size under 4 GiB), no encryption, no data descriptors, no
//!   multi-disk archives, only stored or deflated entries;
//! - entry names are unique, and every entry lies below the central
//!   directory in its own span: offsets are distinct and each local header
//!   plus data ends before the next entry starts;
//! - each entry inflates to exactly its declared size and CRC-32, at most
//!   [`MAX_ENTRY_BYTES`], and declared sizes are checked against a total
//!   budget per read ([`MAX_READ_BYTES`]) or per import
//!   ([`MAX_IMPORT_BYTES`]) before anything is inflated.
use crate::{
    StoreError,
    blob::{BlobKey, BlobStore},
};
use std::{cell::Cell, collections::BTreeMap, io::Read, ops::Range};

/// Largest single entry a reader inflates (a run's logs are capped far
/// below this per step; PRD §17).
pub const MAX_ENTRY_BYTES: u64 = 1 << 30;
/// Inflated bytes one range read of a stored file may produce.
pub const MAX_READ_BYTES: u64 = 1 << 30;
/// Inflated bytes one import may produce (the D3 import's archive cap).
pub const MAX_IMPORT_BYTES: u64 = 4 << 30;
/// Most entries the writer produces: the reader refuses 65,535 (the ZIP64
/// marker), so the writer stops one short and every file it writes reads.
pub const MAX_ENTRIES: usize = 65_534;
const EOCD: u32 = 0x0605_4b50;
const EOCD_LEN: usize = 22;
const ZIP64_LOCATOR: u32 = 0x0706_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const CENTRAL_LEN: usize = 46;
const LOCAL: u32 = 0x0403_4b50;
const LOCAL_LEN: usize = 30;
const MAX_COMMENT: usize = 65_535;
/// First tail read: the end record without a comment, plus slack.
const SHORT_TAIL: u64 = 1024;
/// Flags the writer never sets: encryption (0), data descriptor (3),
/// patched data (5), strong encryption (6), masked headers (13).
const REFUSED_FLAGS: u16 = 1 | 1 << 3 | 1 << 5 | 1 << 6 | 1 << 13;

fn corrupt() -> StoreError {
    StoreError::Backend("archive.corrupt")
}
fn unsupported() -> StoreError {
    StoreError::Backend("archive.unsupported_feature")
}
fn too_large() -> StoreError {
    StoreError::Backend("archive.too_large")
}

/// Write `entries` in the given order. `day` sets every entry's timestamp
/// (midnight of that UTC day, clamped to the DOS range 1980–2107). Refuses
/// anything that would need ZIP64 (`archive.too_large`).
pub fn write(entries: &[(String, Vec<u8>)], day: i64) -> Result<Vec<u8>, StoreError> {
    use ::zip::{CompressionMethod, DateTime, ZipWriter, write::SimpleFileOptions};
    use std::io::Write;
    if entries.len() > MAX_ENTRIES
        || entries
            .iter()
            .any(|(_, bytes)| bytes.len() as u64 >= u64::from(u32::MAX))
    {
        return Err(too_large());
    }
    let (year, month, d) = super::civil(day);
    let time = if (1980..=2107).contains(&year) {
        DateTime::from_date_and_time(year as u16, month as u8, d as u8, 0, 0, 0)
            .map_err(|_| StoreError::Backend("archive.encode_failed"))?
    } else {
        DateTime::default()
    };
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(6))
        .last_modified_time(time)
        .unix_permissions(0o644);
    let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        writer
            .start_file(name.as_str(), options)
            .map_err(|_| StoreError::Backend("archive.encode_failed"))?;
        writer
            .write_all(bytes)
            .map_err(|_| StoreError::Backend("archive.encode_failed"))?;
    }
    let bytes = writer
        .finish()
        .map(std::io::Cursor::into_inner)
        .map_err(|_| StoreError::Backend("archive.encode_failed"))?;
    if bytes.len() as u64 >= u64::from(u32::MAX) {
        return Err(too_large());
    }
    Ok(bytes)
}

/// One central-directory entry, with the end of the span it may occupy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CentralEntry {
    pub name: String,
    pub method: u16,
    pub crc32: u32,
    pub compressed: u64,
    pub size: u64,
    pub offset: u64,
    /// Where the next entry (or the central directory) starts.
    pub span_end: u64,
}

/// Where the central directory is, from the end-of-central-directory record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Directory {
    offset: u64,
    size: u64,
    entries: u64,
}

fn u16_at(b: &[u8], at: usize) -> Result<u16, StoreError> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(corrupt)
}
fn u32_at(b: &[u8], at: usize) -> Result<u32, StoreError> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(corrupt)
}

/// The bytes the full end-record search needs from a `size`-byte file.
fn tail_range(size: u64) -> Range<u64> {
    size.saturating_sub((EOCD_LEN + MAX_COMMENT) as u64)..size
}

/// Find the end record in `tail` (the file's last bytes, starting at
/// `tail_start`). ZIP64 and multi-disk archives are refused.
fn find_end(tail: &[u8], tail_start: u64, size: u64) -> Result<Directory, StoreError> {
    if tail.len() < EOCD_LEN {
        return Err(corrupt());
    }
    let at = (0..=tail.len() - EOCD_LEN)
        .rev()
        .find(|&i| {
            u32_at(tail, i) == Ok(EOCD)
                && usize::from(u16_at(tail, i + 20).unwrap_or(u16::MAX))
                    == tail.len() - i - EOCD_LEN
        })
        .ok_or_else(corrupt)?;
    if at >= 20 && u32_at(tail, at - 20) == Ok(ZIP64_LOCATOR) {
        return Err(unsupported());
    }
    let (disk, cd_disk) = (u16_at(tail, at + 4)?, u16_at(tail, at + 6)?);
    let (on_disk, entries) = (u16_at(tail, at + 8)?, u16_at(tail, at + 10)?);
    let cd_size = u32_at(tail, at + 12)?;
    let cd_offset = u32_at(tail, at + 16)?;
    if entries == u16::MAX || cd_size == u32::MAX || cd_offset == u32::MAX {
        return Err(unsupported());
    }
    if disk != 0 || cd_disk != 0 || on_disk != entries {
        return Err(unsupported());
    }
    let dir = Directory {
        offset: u64::from(cd_offset),
        size: u64::from(cd_size),
        entries: u64::from(entries),
    };
    // The directory ends exactly where the end record starts.
    let end_record = tail_start + at as u64;
    if dir.offset + dir.size != end_record || end_record + EOCD_LEN as u64 > size {
        return Err(corrupt());
    }
    Ok(dir)
}

fn parse_central(cd: &[u8], dir: Directory) -> Result<Vec<CentralEntry>, StoreError> {
    let mut out = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    let mut at = 0;
    while at < cd.len() {
        if u32_at(cd, at)? != CENTRAL {
            return Err(corrupt());
        }
        let flags = u16_at(cd, at + 8)?;
        let method = u16_at(cd, at + 10)?;
        if flags & REFUSED_FLAGS != 0 || !matches!(method, 0 | 8) {
            return Err(unsupported());
        }
        let crc32 = u32_at(cd, at + 16)?;
        let compressed = u32_at(cd, at + 20)?;
        let size = u32_at(cd, at + 24)?;
        let name_len = usize::from(u16_at(cd, at + 28)?);
        let extra_len = usize::from(u16_at(cd, at + 30)?);
        let comment_len = usize::from(u16_at(cd, at + 32)?);
        let disk = u16_at(cd, at + 34)?;
        let offset = u32_at(cd, at + 42)?;
        if [compressed, size, offset].contains(&u32::MAX) || disk != 0 {
            return Err(unsupported());
        }
        let name_at = at + CENTRAL_LEN;
        let name = cd
            .get(name_at..name_at + name_len)
            .and_then(|n| std::str::from_utf8(n).ok())
            .ok_or_else(corrupt)?
            .to_owned();
        let extra = cd
            .get(name_at + name_len..name_at + name_len + extra_len)
            .ok_or_else(corrupt)?;
        let mut e = 0;
        while e + 4 <= extra.len() {
            if u16_at(extra, e)? == 1 {
                return Err(unsupported());
            }
            e += 4 + usize::from(u16_at(extra, e + 2)?);
        }
        if !names.insert(name.clone()) {
            return Err(StoreError::Backend("archive.duplicate_entry"));
        }
        out.push(CentralEntry {
            name,
            method,
            crc32,
            compressed: u64::from(compressed),
            size: u64::from(size),
            offset: u64::from(offset),
            span_end: 0,
        });
        at = name_at + name_len + extra_len + comment_len;
        if at > cd.len() {
            return Err(corrupt());
        }
    }
    if out.len() as u64 != dir.entries {
        return Err(corrupt());
    }
    // Spans: distinct offsets below the directory, each large enough for
    // its own local header and data, never reaching into the next entry.
    let mut order: Vec<usize> = (0..out.len()).collect();
    order.sort_by_key(|&i| out[i].offset);
    for (rank, &i) in order.iter().enumerate() {
        let end = order
            .get(rank + 1)
            .map_or(dir.offset, |&next| out[next].offset);
        let entry = &mut out[i];
        let least = entry.offset + (LOCAL_LEN + entry.name.len()) as u64 + entry.compressed;
        if entry.offset >= dir.offset || least > end {
            return Err(StoreError::Backend("archive.overlapping_entries"));
        }
        entry.span_end = end;
    }
    Ok(out)
}

/// Inflate one entry from `buf`, which holds the file's bytes starting at
/// `buf_start` and covers the entry's span.
fn extract(buf: &[u8], buf_start: u64, entry: &CentralEntry) -> Result<Vec<u8>, StoreError> {
    let at = usize::try_from(entry.offset.checked_sub(buf_start).ok_or_else(corrupt)?)
        .map_err(|_| corrupt())?;
    if u32_at(buf, at)? != LOCAL {
        return Err(corrupt());
    }
    if u16_at(buf, at + 6)? & REFUSED_FLAGS != 0 || u16_at(buf, at + 8)? != entry.method {
        return Err(unsupported());
    }
    let name_len = usize::from(u16_at(buf, at + 26)?);
    let extra_len = usize::from(u16_at(buf, at + 28)?);
    if buf.get(at + LOCAL_LEN..at + LOCAL_LEN + name_len) != Some(entry.name.as_bytes()) {
        return Err(corrupt());
    }
    let data_at = at + LOCAL_LEN + name_len + extra_len;
    let len = usize::try_from(entry.compressed).map_err(|_| corrupt())?;
    let data_end = data_at.checked_add(len).ok_or_else(corrupt)?;
    if buf_start + data_end as u64 > entry.span_end {
        return Err(StoreError::Backend("archive.overlapping_entries"));
    }
    let data = buf.get(data_at..data_end).ok_or_else(corrupt)?;
    if entry.size > MAX_ENTRY_BYTES {
        return Err(too_large());
    }
    let out = match entry.method {
        0 => data.to_vec(),
        _ => {
            let mut out = Vec::with_capacity(entry.size.min(1 << 20) as usize);
            flate2::read::DeflateDecoder::new(data)
                .take(entry.size + 1)
                .read_to_end(&mut out)
                .map_err(|_| corrupt())?;
            out
        }
    };
    let mut crc = flate2::Crc::new();
    crc.update(&out);
    if out.len() as u64 != entry.size || crc.sum() != entry.crc32 {
        return Err(corrupt());
    }
    Ok(out)
}

/// The byte range covering `selected` entries' spans.
fn span(selected: &[&CentralEntry]) -> Option<Range<u64>> {
    let start = selected.iter().map(|e| e.offset).min()?;
    let end = selected.iter().map(|e| e.span_end).max()?;
    Some(start..end)
}

/// Charge `bytes` of declared output against a budget.
fn charge(budget: &Cell<u64>, bytes: u64) -> Result<(), StoreError> {
    let left = budget.get().checked_sub(bytes).ok_or_else(too_large)?;
    budget.set(left);
    Ok(())
}

/// A parsed ZIP held in memory (imports and rewrites), with a total inflate
/// budget across every `read`.
pub struct Zip<'a> {
    bytes: &'a [u8],
    pub entries: Vec<CentralEntry>,
    budget: Cell<u64>,
}
impl<'a> Zip<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, StoreError> {
        Self::parse_with_budget(bytes, MAX_IMPORT_BYTES)
    }
    pub fn parse_with_budget(bytes: &'a [u8], budget: u64) -> Result<Self, StoreError> {
        let size = bytes.len() as u64;
        let tail = tail_range(size);
        let dir = find_end(&bytes[tail.start as usize..], tail.start, size)?;
        let cd = bytes
            .get(dir.offset as usize..(dir.offset + dir.size) as usize)
            .ok_or_else(corrupt)?;
        let entries = parse_central(cd, dir)?;
        // Declared sizes first: a hostile directory fails before inflating.
        let declared = entries.iter().map(|e| e.size).sum::<u64>();
        if declared > budget || entries.iter().any(|e| e.size > MAX_ENTRY_BYTES) {
            return Err(too_large());
        }
        Ok(Self {
            bytes,
            entries,
            budget: Cell::new(budget),
        })
    }
    pub fn read(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.entries
            .iter()
            .find(|e| e.name == name)
            .map(|e| {
                charge(&self.budget, e.size)?;
                extract(self.bytes, 0, e)
            })
            .transpose()
    }
}

/// Read the entries `select` accepts from the stored ZIP at `key` with range
/// reads: the tail, the central directory, then one span covering the
/// selected entries (a run folder is contiguous, so it is one read). Their
/// declared sizes must fit [`MAX_READ_BYTES`].
pub async fn read_entries<B: BlobStore>(
    blobs: &B,
    key: &BlobKey,
    select: impl Fn(&str) -> bool,
) -> Result<BTreeMap<String, Vec<u8>>, StoreError> {
    let size = blobs.head(key).await?.size;
    // Our files carry no comment, so the end record is in the last few
    // bytes; only a foreign file with a comment needs the full search.
    let short = size.saturating_sub(SHORT_TAIL)..size;
    let tail = blobs.get_range(key, short.clone()).await?;
    let dir = match find_end(&tail, short.start, size) {
        Ok(dir) => dir,
        Err(_) if short.start > 0 => {
            let full = tail_range(size);
            let tail = blobs.get_range(key, full.clone()).await?;
            find_end(&tail, full.start, size)?
        }
        Err(e) => return Err(e),
    };
    let cd = blobs
        .get_range(key, dir.offset..dir.offset + dir.size)
        .await?;
    let all = parse_central(&cd, dir)?;
    let selected: Vec<&CentralEntry> = all.iter().filter(|e| select(&e.name)).collect();
    if selected.iter().map(|e| e.size).sum::<u64>() > MAX_READ_BYTES
        || selected.iter().any(|e| e.size > MAX_ENTRY_BYTES)
    {
        return Err(too_large());
    }
    let Some(range) = span(&selected) else {
        return Ok(BTreeMap::new());
    };
    let buf = blobs.get_range(key, range.clone()).await?;
    selected
        .into_iter()
        .map(|e| Ok((e.name.clone(), extract(&buf, range.start, e)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Vec<(String, Vec<u8>)> {
        vec![
            ("manifest.json".into(), b"{}".to_vec()),
            ("exe_a/report.md".into(), b"# A\n".repeat(100)),
            ("exe_a/logs/sr_1.log".into(), b"line\n".to_vec()),
            ("exe_b/report.md".into(), vec![]),
            ("project/manifest.json".into(), b"{}".to_vec()),
        ]
    }
    /// Offset of `name`'s central-directory record in a file we wrote.
    fn central_at(bytes: &[u8], name: &str) -> usize {
        let eocd = bytes.len() - EOCD_LEN;
        let mut at = u32_at(bytes, eocd + 16).unwrap() as usize;
        loop {
            let name_len = usize::from(u16_at(bytes, at + 28).unwrap());
            let extra = usize::from(u16_at(bytes, at + 30).unwrap());
            let comment = usize::from(u16_at(bytes, at + 32).unwrap());
            if &bytes[at + CENTRAL_LEN..at + CENTRAL_LEN + name_len] == name.as_bytes() {
                return at;
            }
            at += CENTRAL_LEN + name_len + extra + comment;
        }
    }
    #[test]
    fn same_entries_give_the_same_bytes_and_read_back() {
        let a = write(&sample(), 20_404).unwrap();
        assert_eq!(a, write(&sample(), 20_404).unwrap());
        assert_ne!(
            a,
            write(&sample(), 20_405).unwrap(),
            "the day is the timestamp"
        );
        let zip = Zip::parse(&a).unwrap();
        assert_eq!(zip.entries.len(), 5);
        for (name, bytes) in sample() {
            assert_eq!(zip.read(&name).unwrap(), Some(bytes), "{name}");
        }
        assert_eq!(zip.read("missing").unwrap(), None);
    }
    #[test]
    fn truncated_or_tampered_bytes_are_corrupt() {
        let a = write(&sample(), 1).unwrap();
        assert!(Zip::parse(&a[..a.len() - 1]).is_err());
        let zip = Zip::parse(&a).unwrap();
        let entry = zip
            .entries
            .iter()
            .find(|e| e.name == "exe_a/report.md")
            .unwrap();
        let mut bad = a.clone();
        let at = entry.offset as usize + LOCAL_LEN + entry.name.len() + 2;
        bad[at] ^= 0xFF;
        assert!(Zip::parse(&bad).unwrap().read("exe_a/report.md").is_err());
        assert!(Zip::parse(b"not a zip").is_err());
        assert!(Zip::parse(b"").is_err());
    }
    #[test]
    fn hostile_directories_are_refused() {
        let good = write(&sample(), 1).unwrap();
        // An oversized declared size: refused before inflating anything.
        let mut big = good.clone();
        let at = central_at(&big, "exe_a/report.md");
        big[at + 24..at + 28].copy_from_slice(&(u32::MAX - 1).to_le_bytes());
        assert_eq!(Zip::parse(&big).err(), Some(too_large()));
        // A total budget across entries.
        assert_eq!(Zip::parse_with_budget(&good, 100).err(), Some(too_large()));
        let small = Zip::parse_with_budget(&good, 410).unwrap();
        assert!(small.read("exe_a/report.md").is_ok());
        assert_eq!(small.read("exe_a/report.md").err(), Some(too_large()));
        let two = write(
            &[("a".into(), b"one".to_vec()), ("b".into(), b"two".to_vec())],
            1,
        )
        .unwrap();
        // A duplicate name.
        let mut renamed = two.clone();
        let at = central_at(&renamed, "b");
        renamed[at + CENTRAL_LEN] = b'a';
        assert_eq!(
            Zip::parse(&renamed).err(),
            Some(StoreError::Backend("archive.duplicate_entry"))
        );
        // Overlapping offsets: two entries pointing at the same local header.
        let mut overlap = two.clone();
        let at = central_at(&overlap, "b");
        overlap[at + 42..at + 46].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            Zip::parse(&overlap).err(),
            Some(StoreError::Backend("archive.overlapping_entries"))
        );
        // An offset into the central directory itself.
        let mut inside = two.clone();
        let at = central_at(&inside, "b");
        inside[at + 42..at + 46].copy_from_slice(&(at as u32).to_le_bytes());
        assert_eq!(
            Zip::parse(&inside).err(),
            Some(StoreError::Backend("archive.overlapping_entries"))
        );
        // A truncated central directory: the count says more than it holds,
        // or the size cuts a record short.
        let mut short = two.clone();
        let eocd = short.len() - EOCD_LEN;
        short[eocd + 8..eocd + 12].copy_from_slice(&[3, 0, 3, 0]);
        assert!(Zip::parse(&short).is_err());
        let mut cut = two.clone();
        let eocd = cut.len() - EOCD_LEN;
        let size = u32_at(&cut, eocd + 12).unwrap() - 10;
        cut[eocd + 12..eocd + 16].copy_from_slice(&size.to_le_bytes());
        assert!(Zip::parse(&cut).is_err());
        // Encrypted entries and ZIP64 markers are unsupported, not guessed at.
        let mut encrypted = two.clone();
        let at = central_at(&encrypted, "a");
        encrypted[at + 8] |= 1;
        assert_eq!(Zip::parse(&encrypted).err(), Some(unsupported()));
        let mut zip64 = two.clone();
        let eocd = zip64.len() - EOCD_LEN;
        zip64[eocd + 8..eocd + 12].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(Zip::parse(&zip64).err(), Some(unsupported()));
    }
    #[test]
    fn the_writer_refuses_what_would_need_zip64() {
        let many: Vec<(String, Vec<u8>)> = (0..=MAX_ENTRIES)
            .map(|n| (format!("{n}"), vec![]))
            .collect();
        assert_eq!(write(&many, 1).err(), Some(too_large()));
    }
}
