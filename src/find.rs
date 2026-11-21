use std::{
    ffi::OsString,
    mem,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Globalization::{CSTR_EQUAL, CompareStringOrdinal, FIND_FROMSTART, FindStringOrdinal},
        Storage::FileSystem::{
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, GetFileAttributesW,
            INVALID_FILE_ATTRIBUTES,
        },
    },
    core::PCWSTR,
};

use crate::directory::{DirectoryEntries, extended_path};

const BATCH_SIZE: usize = 256;
const BATCH_INTERVAL: Duration = Duration::from_millis(32);

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

pub struct Hit {
    pub folder: Arc<PathBuf>,
    pub name: Box<[u16]>,
    pub pane: usize,
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
    query: &Query,
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
                } else if query.matches(bare) {
                    let folder = frame.folder.get_or_insert_with(|| Arc::new(path.clone()));
                    batch.push(Hit {
                        folder: folder.clone(),
                        name: Box::from(name),
                        pane,
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
}
