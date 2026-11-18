use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io,
    os::windows::{ffi::OsStringExt, fs::OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

use windows::Win32::Storage::FileSystem::{
    DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
    FILE_SHARE_READ, FILE_SHARE_WRITE,
};

use crate::{
    compare::{self, Identity, Index, Stamp, ordinal},
    directory::{self, Entry},
    undo,
};

pub struct Selection {
    pub folder: PathBuf,
    pub entries: Vec<Entry>,
}

pub struct Rules {
    pub find: Vec<u16>,
    pub replace: Vec<u16>,
    pub prefix: Vec<u16>,
    pub suffix: Vec<u16>,
    pub numbering: Option<(u32, usize)>,
    pub keep_extension: bool,
}

impl Rules {
    fn target(&self, name: &[u16], index: usize) -> Box<[u16]> {
        let name = &name[..name.len() - 1];
        let split = self
            .keep_extension
            .then(|| name.iter().rposition(|&unit| unit == u16::from(b'.')))
            .flatten()
            .filter(|&position| position > 0)
            .unwrap_or(name.len());
        let (stem, extension) = name.split_at(split);
        let mut output = self.prefix.clone();
        if self.find.is_empty() {
            output.extend_from_slice(stem);
        } else {
            let mut remaining = stem;
            while let Some(match_at) = remaining
                .windows(self.find.len())
                .position(|part| part == self.find)
            {
                output.extend_from_slice(&remaining[..match_at]);
                output.extend_from_slice(&self.replace);
                // One extra unit is enough to report a Windows length violation.
                if output.len() > 255 {
                    output.truncate(256);
                    break;
                }
                remaining = &remaining[match_at + self.find.len()..];
            }
            if output.len() <= 255 {
                output.extend_from_slice(remaining);
            }
        }
        output.extend_from_slice(&self.suffix);
        if let Some((first, width)) = self.numbering {
            let number = u64::from(first) + index as u64;
            output.extend(format!(" {number:0width$}").encode_utf16());
        }
        output.extend_from_slice(extension);
        output.truncate(256);
        output.push(0);
        output.into_boxed_slice()
    }
}

#[derive(PartialEq, Eq)]
pub enum Check {
    Ready,
    Unchanged,
    Unsupported,
    Invalid(&'static str),
    Duplicate,
    Occupied,
    SourceChanged,
    Unavailable(String),
}

impl Check {
    pub fn label(&self) -> &str {
        match self {
            Self::Ready => "Ready",
            Self::Unchanged => "Unchanged",
            Self::Unsupported => "Select regular files only",
            Self::Invalid(reason) => reason,
            Self::Duplicate => "Duplicate proposed name",
            Self::Occupied => "Destination name exists",
            Self::SourceChanged => "Source changed · reopen with the current selection",
            Self::Unavailable(reason) => reason,
        }
    }
}

pub struct Row {
    pub target: Box<[u16]>,
    pub check: Check,
    identity: Option<Identity>,
}

pub struct Plan {
    pub selection: Arc<Selection>,
    pub rows: Vec<Row>,
    pub ready: usize,
    pub conflicts: usize,
    root: Identity,
}

pub enum Outcome {
    Unchanged,
    NotAttempted,
    Renamed,
    Failed(String),
}

impl Outcome {
    pub fn label(&self) -> &str {
        match self {
            Self::Unchanged => "Unchanged",
            Self::NotAttempted => "Not attempted",
            Self::Renamed => "Renamed",
            Self::Failed(reason) => reason,
        }
    }
}

pub struct Report {
    pub rows: Vec<Outcome>,
    pub renamed: usize,
    pub reason: Option<String>,
    pub undo: Vec<undo::Receipt>,
}

impl Report {
    pub fn stopped(plan: &Plan, reason: String) -> Self {
        Self {
            rows: plan
                .rows
                .iter()
                .map(|row| match row.check {
                    Check::Unchanged => Outcome::Unchanged,
                    _ => Outcome::NotAttempted,
                })
                .collect(),
            renamed: 0,
            reason: Some(reason),
            undo: Vec::new(),
        }
    }

    fn fail(&mut self, index: usize, before_changes: bool, error: impl std::fmt::Display) {
        let detail = format!("{error}");
        self.rows[index] = Outcome::Failed(detail.clone());
        self.reason = Some(if before_changes {
            format!("Review is stale or unavailable · {detail}")
        } else {
            format!("Batch stopped · {detail}")
        });
    }
}

fn check_current(current: &impl Fn() -> bool) -> io::Result<()> {
    if !current() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    Ok(())
}

fn named_path(folder: &Path, name: &[u16]) -> PathBuf {
    folder.join(OsString::from_wide(&name[..name.len() - 1]))
}

fn read(folder: &Path, current: &impl Fn() -> bool) -> io::Result<Vec<Entry>> {
    check_current(current)?;
    directory::read(folder, current, |_| {}).map_err(io::Error::other)
}

fn source_is_current(index: &Index<'_>, original: &Entry) -> bool {
    index
        .unique(&original.name)
        .ok()
        .flatten()
        .is_some_and(|file| {
            file.name == original.name && Stamp::entry(file) == Stamp::entry(original)
        })
}

fn occupied(
    index: &Index<'_>,
    folder: &Path,
    original: &Entry,
    target: &[u16],
) -> io::Result<bool> {
    match index.unique(target) {
        // Case-only changes resolve to our own exact source name.
        Ok(Some(entry)) => Ok(entry.name != original.name),
        Err(()) => Ok(true),
        // Native lookup also catches aliases absent from the long-name listing.
        Ok(None) => Ok(compare::path_stamp(&named_path(folder, target))?.is_some()),
    }
}

pub fn review(
    selection: Arc<Selection>,
    rules: Rules,
    current: impl Fn() -> bool,
) -> io::Result<Plan> {
    check_current(&current)?;
    let folder = &selection.folder;
    let root = compare::directory_identity(folder)?;
    let entries = read(folder, &current)?;
    let names = Index::new(&entries, &current)?;

    let mut rows = Vec::with_capacity(selection.entries.len());
    for (index, original) in selection.entries.iter().enumerate() {
        check_current(&current)?;
        let target = rules.target(&original.name, index);
        let check = if !Stamp::entry(original).regular()
            || directory::filename_issue(&original.name[..original.name.len() - 1]).is_some()
        {
            Check::Unsupported
        } else if target == original.name {
            Check::Unchanged
        } else if let Some(reason) = directory::filename_issue(&target[..target.len() - 1]) {
            Check::Invalid(reason)
        } else {
            Check::Ready
        };
        rows.push(Row {
            target,
            check,
            identity: None,
        });
    }
    let mut changing: Vec<_> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, row)| (row.check == Check::Ready).then_some(i))
        .collect();
    changing.sort_unstable_by(|&a, &b| ordinal(&rows[a].target, &rows[b].target));
    for pair in changing.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if ordinal(&rows[a].target, &rows[b].target).is_eq() {
            rows[a].check = Check::Duplicate;
            rows[b].check = Check::Duplicate;
        }
    }
    let mut ready = 0;
    let mut conflicts = 0;
    for (row, original) in rows.iter_mut().zip(&selection.entries) {
        check_current(&current)?;
        if row.check == Check::Ready {
            if !source_is_current(&names, original) {
                row.check = Check::SourceChanged;
            } else {
                match occupied(&names, folder, original, &row.target) {
                    Ok(true) => row.check = Check::Occupied,
                    Err(error) => row.check = Check::Unavailable(error.to_string()),
                    Ok(false) => {
                        match compare::regular_identity(&named_path(folder, &original.name)) {
                            Ok((id, stamp)) if stamp == Stamp::entry(original) => {
                                row.identity = Some(id)
                            }
                            Ok(_) => row.check = Check::SourceChanged,
                            Err(error) => row.check = Check::Unavailable(error.to_string()),
                        }
                    }
                }
            }
        }
        match row.check {
            Check::Ready => ready += 1,
            Check::Unchanged => {}
            _ => conflicts += 1,
        }
    }
    check_current(&current)?;
    if compare::directory_identity(folder)? != root {
        return Err(io::Error::other(
            "The selected folder changed. Reopen Batch Rename.",
        ));
    }
    Ok(Plan {
        selection,
        rows,
        ready,
        conflicts,
        root,
    })
}

pub fn apply(plan: &Plan) -> Report {
    let mut report = Report::stopped(plan, "Batch stopped".into());
    if plan.conflicts != 0 || plan.ready == 0 {
        return report;
    }
    let result = locked_folder(plan).and_then(|(lock, path)| {
        let entries = read(&path, &|| true)?;
        Ok((lock, path, entries))
    });
    let (_folder_lock, folder, entries) = match result {
        Ok(value) => value,
        Err(error) => {
            report.reason = Some(format!("Could not recheck the folder · {error}"));
            return report;
        }
    };
    let names = match Index::new(&entries, || true) {
        Ok(names) => names,
        Err(error) => {
            report.reason = Some(format!("Could not recheck the names · {error}"));
            return report;
        }
    };
    for (index, (row, original)) in plan.rows.iter().zip(&plan.selection.entries).enumerate() {
        let Some(identity) = row.identity else {
            continue;
        };
        let result = (|| {
            if !source_is_current(&names, original) {
                return Err(io::Error::other("The source name or metadata changed."));
            }
            if occupied(&names, &folder, original, &row.target)? {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "The destination name now exists.",
                ));
            }
            let current = compare::regular_identity(&named_path(&folder, &original.name))?;
            if current != (identity, Stamp::entry(original)) {
                return Err(io::Error::other(
                    "The source file was replaced or modified.",
                ));
            }
            Ok(())
        })();
        if let Err(error) = result {
            report.fail(index, true, error);
            return report;
        }
    }
    for (index, (row, original)) in plan.rows.iter().zip(&plan.selection.entries).enumerate() {
        let Some(identity) = row.identity else {
            continue;
        };
        match rename(
            &folder,
            original,
            &row.target,
            identity,
            plan,
            report.undo.len() < undo::MAX_ITEMS,
        ) {
            Ok(receipt) => report.undo.extend(receipt),
            Err(error) => {
                report.fail(index, false, error);
                return report;
            }
        }
        report.rows[index] = Outcome::Renamed;
        report.renamed += 1;
    }
    report.reason = None;
    report
}

fn locked_folder(plan: &Plan) -> io::Result<(File, PathBuf)> {
    let lock = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES.0)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(&plan.selection.folder)?;
    if compare::directory_handle_identity(&lock)? != plan.root {
        return Err(io::Error::other("The reviewed folder was replaced."));
    }
    let path = undo::final_path(&lock)?;
    Ok((lock, path))
}

fn rename(
    folder: &Path,
    original: &Entry,
    target: &[u16],
    identity: Identity,
    plan: &Plan,
    record: bool,
) -> io::Result<Option<undo::Receipt>> {
    let file = OpenOptions::new()
        .access_mode((DELETE | FILE_READ_ATTRIBUTES).0)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(named_path(folder, &original.name))?;
    if compare::regular_handle_identity(&file)? != (identity, Stamp::entry(original)) {
        return Err(io::Error::other(
            "The source file was replaced or modified.",
        ));
    }

    let candidate = if record {
        Some(undo::Candidate::locked(
            named_path(&plan.selection.folder, &original.name),
            named_path(&plan.selection.folder, target),
            plan.root,
            &file,
        ))
        .flatten()
    } else {
        None
    };
    undo::rename_handle(&file, &named_path(folder, target))?;
    Ok(candidate.and_then(|candidate| candidate.confirm_handle(&file)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn late_destination_aborts_the_whole_batch_and_case_only_rename_works() -> io::Result<()> {
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let path = std::env::temp_dir().join(format!(
            "dual-pane-rename-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path)?;
        let fixture = Fixture(path);
        let path = &fixture.0;
        fs::write(path.join("alpha.txt"), b"alpha")?;
        fs::write(path.join("beta.txt"), b"beta")?;
        let mut entries = read(path, &|| true)?;
        entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        let selection = Arc::new(Selection {
            folder: path.clone(),
            entries,
        });
        let rules = |find: &str, replace: &str, prefix: &str| Rules {
            find: find.encode_utf16().collect(),
            replace: replace.encode_utf16().collect(),
            prefix: prefix.encode_utf16().collect(),
            suffix: Vec::new(),
            numbering: None,
            keep_extension: true,
        };
        let plan = review(selection.clone(), rules("", "", "new-"), || true)?;
        assert_eq!(plan.ready, 2);
        fs::write(path.join("new-beta.txt"), b"occupied")?;
        let report = apply(&plan);
        assert_eq!(report.renamed, 0);
        assert!(matches!(report.rows[0], Outcome::NotAttempted));
        assert!(matches!(report.rows[1], Outcome::Failed(_)));
        assert_eq!(fs::read(path.join("new-beta.txt"))?, b"occupied");
        assert!(!path.join("new-alpha.txt").exists());

        let plan = review(selection, rules("alpha", "ALPHA", ""), || true)?;
        assert_eq!(plan.ready, 1);
        let report = apply(&plan);
        assert_eq!(report.renamed, 1, "{:?}", report.reason);
        assert!(
            read(path, &|| true)?
                .iter()
                .any(|entry| entry.filename() == "ALPHA.txt")
        );
        assert_eq!(fs::read(path.join("ALPHA.txt"))?, b"alpha");
        Ok(())
    }
}
