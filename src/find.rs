use std::{
    ffi::OsString,
    fs::File,
    io::Read,
    mem,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::FILETIME,
        Globalization::{CSTR_EQUAL, CompareStringOrdinal, FIND_FROMSTART, FindStringOrdinal},
        Storage::FileSystem::{
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, GetFileAttributesW,
            INVALID_FILE_ATTRIBUTES,
        },
        System::SystemInformation::GetSystemTimeAsFileTime,
    },
    core::PCWSTR,
};

use crate::directory::{DirectoryEntries, extended_path};

const BATCH_SIZE: usize = 256;
const BATCH_INTERVAL: Duration = Duration::from_millis(32);
const CONTENT_CHUNK: usize = 64 * 1024;
const CONTENT_MAX_BYTES: u64 = 16 * 1024 * 1024;
const BINARY_SNIFF_BYTES: usize = 4096;
const TICKS_PER_DAY: u64 = 86_400 * 10_000_000;

pub struct Query {
    pattern: Vec<u16>,
    wildcard: bool,
}

impl Query {
    pub fn new(mut pattern: Vec<u16>) -> Self {
        pattern.dedup_by(|right, left| *left == u16::from(b'*') && left == right);
        let wildcard = pattern.iter().any(|&unit| matches!(unit, 42 | 63));
        Self { pattern, wildcard }
    }

    pub fn matches(&self, name: &[u16]) -> bool {
        if self.wildcard {
            glob(&self.pattern, name)
        } else {
            unsafe { FindStringOrdinal(FIND_FROMSTART, name, &self.pattern, true) >= 0 }
        }
    }
}

fn character_end(text: &[u16], start: usize) -> usize {
    let pair = (0xd800..=0xdbff).contains(&text[start])
        && text
            .get(start + 1)
            .is_some_and(|next| (0xdc00..=0xdfff).contains(next));
    start + 1 + usize::from(pair)
}

fn glob(pattern: &[u16], name: &[u16]) -> bool {
    let (mut p, mut n) = (0, 0);
    let mut star = None;
    while n < name.len() {
        let end = character_end(name, n);
        if let Some(&unit) = pattern.get(p) {
            if unit == u16::from(b'*') {
                p += 1;
                star = Some((p, n));
                continue;
            }
            let pattern_end = character_end(pattern, p);
            let first = name[n];
            let equal = if unit <= 0x7f && first <= 0x7f {
                (unit as u8).eq_ignore_ascii_case(&(first as u8))
            } else {
                (unsafe { CompareStringOrdinal(&pattern[p..pattern_end], &name[n..end], true) })
                    == CSTR_EQUAL
            };
            if unit == u16::from(b'?') || equal {
                p = pattern_end;
                n = end;
                continue;
            }
        }
        let Some((resume, previous)) = star else {
            return false;
        };
        n = character_end(name, previous);
        p = resume;
        star = Some((resume, n));
    }
    pattern[p..].iter().all(|&unit| unit == u16::from(b'*'))
}

pub struct ContentQuery {
    needle: String,
    needle_bytes: Vec<u8>,
    ascii: bool,
    case_sensitive: bool,
}

impl ContentQuery {
    pub fn new(text: &str, case_sensitive: bool) -> Self {
        let needle = if case_sensitive {
            text.to_owned()
        } else {
            text.to_lowercase()
        };
        let ascii = needle.is_ascii();
        let needle_bytes = needle.as_bytes().to_vec();
        Self {
            needle,
            needle_bytes,
            ascii,
            case_sensitive,
        }
    }

    pub fn matches_file(&self, path: &Path, current: &impl Fn() -> bool) -> bool {
        if self.needle_bytes.is_empty() || !current() {
            return false;
        }
        let Ok(mut file) = File::open(path) else {
            return false;
        };
        let mut buf = vec![0_u8; CONTENT_CHUNK];
        let mut total = 0_u64;
        let mut first = true;
        let mut utf16le = false;
        let mut tail = Vec::new();
        let overlap = self.needle_bytes.len().saturating_sub(1);

        while total < CONTENT_MAX_BYTES {
            if !current() {
                return false;
            }
            let remaining = (CONTENT_MAX_BYTES - total).min(buf.len() as u64) as usize;
            let read = match file.read(&mut buf[..remaining]) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => return false,
            };
            total += read as u64;
            let chunk = &buf[..read];
            if first {
                first = false;
                if chunk.starts_with(&[0xFF, 0xFE]) {
                    utf16le = true;
                } else if chunk[..chunk.len().min(BINARY_SNIFF_BYTES)].contains(&0) {
                    return false;
                }
            }
            if utf16le {
                tail.extend_from_slice(chunk);
                let start = if total == read as u64 { 2 } else { 0 };
                let usable = tail.len() - (tail.len() - start) % 2;
                let units: Vec<u16> = tail[start..usable]
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                let text = String::from_utf16_lossy(&units);
                if self.matches_str(&text) {
                    return true;
                }
                let keep_bytes = self
                    .needle
                    .encode_utf16()
                    .count()
                    .saturating_sub(1)
                    .saturating_mul(2)
                    + (tail.len() - usable);
                if tail.len() > keep_bytes {
                    tail.drain(..tail.len() - keep_bytes);
                }
            } else if self.ascii {
                tail.extend_from_slice(chunk);
                if self.matches_ascii_bytes(&tail) {
                    return true;
                }
                if tail.len() > overlap {
                    tail.drain(..tail.len() - overlap);
                }
            } else {
                tail.extend_from_slice(chunk);
                let text = String::from_utf8_lossy(&tail);
                if self.matches_str(&text) {
                    return true;
                }
                if tail.len() > overlap {
                    let mut cut = tail.len() - overlap;
                    while cut < tail.len() && (tail[cut] & 0xC0) == 0x80 {
                        cut += 1;
                    }
                    tail.drain(..cut);
                }
            }
        }
        false
    }

    fn matches_str(&self, text: &str) -> bool {
        if self.case_sensitive {
            text.contains(&self.needle)
        } else {
            text.to_lowercase().contains(&self.needle)
        }
    }

    fn matches_ascii_bytes(&self, haystack: &[u8]) -> bool {
        let needle = self.needle_bytes.as_slice();
        if needle.len() > haystack.len() {
            return false;
        }
        if self.case_sensitive {
            haystack
                .windows(needle.len())
                .any(|window| window == needle)
        } else {
            haystack
                .windows(needle.len())
                .any(|window| window.eq_ignore_ascii_case(needle))
        }
    }
}

#[derive(Default)]
pub struct SearchFilter {
    pub name: Option<Query>,
    pub content: Option<ContentQuery>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub modified_after: Option<u64>,
}

pub fn cutoff_filetime_days(days: u64) -> u64 {
    let now = unsafe { GetSystemTimeAsFileTime() };
    let ticks = (u64::from(now.dwHighDateTime) << 32) | u64::from(now.dwLowDateTime);
    ticks.saturating_sub(days.saturating_mul(TICKS_PER_DAY))
}

pub struct Hit {
    pub folder: Arc<PathBuf>,
    pub name: Box<[u16]>,
    pub pane: usize,
    pub size: u64,
    pub modified: FILETIME,
}

#[derive(Default)]
pub struct Report {
    pub unreadable: usize,
    pub links: usize,
    pub first_error: Option<String>,
}

impl Report {
    fn error(&mut self, path: &Path, error: windows::core::Error) {
        self.unreadable += 1;
        if self.first_error.is_none() {
            self.first_error = Some(format!("{} · {}", path.display(), error.message().trim()));
        }
    }
}

pub enum Update {
    Hits(Vec<Hit>),
    Finished(Report),
}

struct Frame {
    entries: DirectoryEntries,
    folder: Option<Arc<PathBuf>>,
}

impl Frame {
    fn open(path: &Path) -> windows::core::Result<Self> {
        Ok(Self {
            entries: DirectoryEntries::open(path)?,
            folder: None,
        })
    }
}

pub fn scan(
    mut path: PathBuf,
    pane: usize,
    filter: &SearchFilter,
    current: impl Fn() -> bool,
    mut publish: impl FnMut(Update) -> bool,
) {
    let mut report = Report::default();
    let remote = matches!(
        path.components().next(),
        Some(std::path::Component::Prefix(prefix))
            if matches!(prefix.kind(), std::path::Prefix::UNC(..) | std::path::Prefix::VerbatimUNC(..))
    );
    if !current() {
        return;
    }
    // Drive and UNC share roots have no final name. GetFileAttributesW does not
    // support querying a network share root directly.
    if path.file_name().is_some() {
        let wide = extended_path(&path);
        let attributes = unsafe { GetFileAttributesW(PCWSTR(wide.as_ptr())) };
        if attributes == INVALID_FILE_ATTRIBUTES {
            report.error(&path, windows::core::Error::from_thread());
            publish(Update::Finished(report));
            return;
        }
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            report.links = 1;
            publish(Update::Finished(report));
            return;
        }
    }
    if !current() {
        return;
    }
    let mut stack = match Frame::open(&path) {
        Ok(root) => vec![root],
        Err(error) => {
            report.error(&path, error);
            publish(Update::Finished(report));
            return;
        }
    };
    let mut batch = Vec::new();
    let mut last_update = None::<Instant>;
    while let Some(frame) = stack.last_mut() {
        if !current() {
            return;
        }
        if !batch.is_empty()
            && (batch.len() >= BATCH_SIZE
                || last_update.is_none_or(|last| last.elapsed() >= BATCH_INTERVAL))
        {
            if !publish(Update::Hits(mem::take(&mut batch))) {
                return;
            }
            last_update = Some(Instant::now());
        }
        match frame.entries.next() {
            Ok(Some((name, data))) => {
                let attributes = data.dwFileAttributes;
                let bare = &name[..name.len() - 1];
                if !current() {
                    return;
                }
                if attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
                    if attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
                        report.links += 1;
                        continue;
                    }
                    path.push(OsString::from_wide(bare));
                    // Publish known matches before an open that might stall.
                    if remote && !batch.is_empty() {
                        if !publish(Update::Hits(mem::take(&mut batch))) {
                            return;
                        }
                        last_update = Some(Instant::now());
                    }
                    if !current() {
                        return;
                    }
                    match Frame::open(&path) {
                        Ok(child) => stack.push(child),
                        Err(error) => {
                            report.error(&path, error);
                            path.pop();
                        }
                    }
                } else if filter.name.as_ref().is_none_or(|query| query.matches(bare)) {
                    let size = (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow);
                    if filter.min_size.is_some_and(|min| size < min)
                        || filter.max_size.is_some_and(|max| size > max)
                    {
                        continue;
                    }
                    if let Some(cutoff) = filter.modified_after {
                        let modified = (u64::from(data.ftLastWriteTime.dwHighDateTime) << 32)
                            | u64::from(data.ftLastWriteTime.dwLowDateTime);
                        if modified < cutoff {
                            continue;
                        }
                    }
                    if let Some(content) = &filter.content {
                        let file_path = path.join(OsString::from_wide(bare));
                        if !content.matches_file(&file_path, &current) {
                            continue;
                        }
                    }
                    let folder = frame.folder.get_or_insert_with(|| Arc::new(path.clone()));
                    batch.push(Hit {
                        folder: folder.clone(),
                        name: Box::from(name),
                        pane,
                        size,
                        modified: data.ftLastWriteTime,
                    });
                }
                continue;
            }
            Ok(None) => {}
            Err(error) => report.error(&path, error),
        }
        stack.pop();
        path.pop();
    }
    if current() && (batch.is_empty() || publish(Update::Hits(batch))) {
        publish(Update::Finished(report));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_substrings_and_wildcards_without_changing_unicode_names() {
        for (pattern, name, expected) in [
            ("flAc", "01.FLAC", true),
            ("ÅS", "påske.txt", true),
            ("*.flac", "01.FLAC", true),
            ("*.flac", "01.flac.bak", false),
            ("?é**.txt", "🦀É.txt", true),
            ("?é*.txt", "abé.txt", false),
            ("*a*b", "AxazB", true),
            ("a*b", "xaB", false),
            ("*", "readme", true),
        ] {
            let query = Query::new(pattern.encode_utf16().collect());
            assert_eq!(
                query.matches(&name.encode_utf16().collect::<Vec<_>>()),
                expected,
                "{pattern}, {name}"
            );
        }
    }

    #[test]
    fn content_search_matches_utf8_utf16_boundaries_and_skips_binary() {
        let dir = tempfile::tempdir().unwrap();
        let text_file = dir.path().join("notes.txt");
        let mut spanned = vec![b'x'; CONTENT_CHUNK - 3];
        spanned.extend_from_slice(b"NeedleMatch");
        std::fs::write(&text_file, &spanned).unwrap();

        let binary_file = dir.path().join("data.bin");
        std::fs::write(&binary_file, b"\x00\x01\x02NeedleMatch").unwrap();

        let utf16_file = dir.path().join("wide.txt");
        let mut utf16 = vec![0xFF_u8, 0xFE];
        for unit in "Hello NeedleMatch".encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(&utf16_file, &utf16).unwrap();

        let insensitive = ContentQuery::new("needlematch", false);
        assert!(insensitive.matches_file(&text_file, &|| true));
        assert!(!insensitive.matches_file(&binary_file, &|| true));
        assert!(insensitive.matches_file(&utf16_file, &|| true));

        let sensitive = ContentQuery::new("needlematch", true);
        assert!(!sensitive.matches_file(&text_file, &|| true));
        let exact = ContentQuery::new("NeedleMatch", true);
        assert!(exact.matches_file(&text_file, &|| true));
    }

    #[test]
    fn scan_applies_size_and_content_filters() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("small.txt"), "hello world").unwrap();
        std::fs::write(dir.path().join("large.txt"), "a".repeat(2048) + "hello").unwrap();
        std::fs::write(dir.path().join("other.txt"), "a".repeat(2048)).unwrap();

        let filter = SearchFilter {
            name: Some(Query::new("*.txt".encode_utf16().collect())),
            content: Some(ContentQuery::new("HELLO", false)),
            min_size: Some(1024),
            max_size: None,
            modified_after: None,
        };
        let mut hits = Vec::new();
        scan(
            dir.path().to_path_buf(),
            0,
            &filter,
            || true,
            |update| {
                if let Update::Hits(batch) = update {
                    hits.extend(batch);
                }
                true
            },
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(
            OsString::from_wide(&hits[0].name[..hits[0].name.len() - 1]),
            "large.txt"
        );
    }
}
