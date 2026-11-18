use std::{
    collections::HashMap,
    ffi::OsString,
    fs::{File, OpenOptions},
    io,
    mem::offset_of,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::{Path, PathBuf},
};

use windows::Win32::{
    Foundation::HANDLE,
    Storage::FileSystem::{
        DELETE, FILE_ATTRIBUTE_DEVICE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
        FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_RENAME_INFO, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FileBasicInfo, FileIdInfo, FileRenameInfo,
        GetFileInformationByHandleEx, GetFinalPathNameByHandleW, SetFileInformationByHandle,
    },
};

use crate::{
    compare::{self, Identity},
    directory::{self, extended_path},
};

mod store;
pub use store::{Applied, Group, Kind, Store};

pub const MAX_ITEMS: usize = 20_000;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Proof {
    volume: u64,
    id: [u8; 16],
    created: u64,
    modified: u64,
    changed: i64,
    bytes: u64,
    attributes: u32,
}

impl Proof {
    fn file(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        let attributes = metadata.file_attributes();
        if attributes
            & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DEVICE).0
            != 0
        {
            return Err(io::Error::other("The item is not a regular file."));
        }
        let handle = HANDLE(file.as_raw_handle());
        let mut identity = FILE_ID_INFO::default();
        let mut basic = FILE_BASIC_INFO::default();
        unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileIdInfo,
                (&mut identity as *mut FILE_ID_INFO).cast(),
                size_of::<FILE_ID_INFO>() as u32,
            )
            .map_err(io::Error::other)?;
            GetFileInformationByHandleEx(
                handle,
                FileBasicInfo,
                (&mut basic as *mut FILE_BASIC_INFO).cast(),
                size_of::<FILE_BASIC_INFO>() as u32,
            )
            .map_err(io::Error::other)?;
        }
        if identity.FileId.Identifier == [0; 16] {
            return Err(io::Error::other("A stable file identifier is unavailable."));
        }
        Ok(Self {
            volume: identity.VolumeSerialNumber,
            id: identity.FileId.Identifier,
            created: metadata.creation_time(),
            modified: metadata.last_write_time(),
            changed: basic.ChangeTime,
            bytes: metadata.file_size(),
            attributes,
        })
    }

    fn same_payload(self, other: Self) -> bool {
        self.volume == other.volume
            && self.id == other.id
            && self.created == other.created
            && self.modified == other.modified
            && self.bytes == other.bytes
    }
}

#[derive(Clone)]
pub struct Receipt {
    pub from: PathBuf,
    pub to: PathBuf,
    roots: [Identity; 2],
    before: Proof,
    after: Proof,
    restored: bool,
}

impl Receipt {
    pub fn restored(&self) -> bool {
        self.restored
    }
}

pub struct Candidate {
    from: PathBuf,
    to: PathBuf,
    roots: [Identity; 2],
    before: Proof,
}

impl Candidate {
    pub fn capture(from: &Path, to: &Path) -> Option<Self> {
        let before = inspect_named(from).ok()?;
        let roots = [
            compare::directory_identity(from.parent()?).ok()?,
            compare::directory_identity(to.parent()?).ok()?,
        ];
        Self::new(from.to_owned(), to.to_owned(), roots, before).ok()
    }

    pub fn locked(from: PathBuf, to: PathBuf, root: Identity, file: &File) -> Option<Self> {
        Self::new(from, to, [root; 2], Proof::file(file).ok()?).ok()
    }

    fn new(from: PathBuf, to: PathBuf, roots: [Identity; 2], before: Proof) -> io::Result<Self> {
        if !valid_path(&from)
            || !valid_path(&to)
            || (from.file_name() == to.file_name() && roots[0] == roots[1])
        {
            return Err(io::Error::other("The paths cannot be recorded for undo."));
        }
        let candidate = Self {
            from,
            to,
            roots,
            before,
        };
        if !free_name(&candidate.from, &candidate.to, roots, before)? {
            return Err(io::Error::other("The destination name is occupied."));
        }
        Ok(candidate)
    }

    pub fn destination_empty(&self) -> bool {
        matches!(compare::path_stamp(&self.to), Ok(None))
    }

    pub fn confirm(self, actual: &Path) -> Option<Receipt> {
        if actual.file_name() != self.to.file_name()
            || compare::directory_identity(actual.parent()?).ok()? != self.roots[1]
        {
            return None;
        }
        let after = inspect_named(actual).ok()?;
        self.confirm_proof(after)
    }

    pub fn confirm_handle(self, file: &File) -> Option<Receipt> {
        self.confirm_proof(Proof::file(file).ok()?)
    }

    fn confirm_proof(self, after: Proof) -> Option<Receipt> {
        self.before.same_payload(after).then_some(Receipt {
            from: self.from,
            to: self.to,
            roots: self.roots,
            before: self.before,
            after,
            restored: false,
        })
    }
}

fn valid_path(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    path.is_absolute()
        && path.file_name().is_some()
        && path.components().all(|part| match part {
            Component::Prefix(prefix) => matches!(
                prefix.kind(),
                Prefix::Disk(_)
                    | Prefix::UNC(..)
                    | Prefix::VerbatimDisk(_)
                    | Prefix::VerbatimUNC(..)
            ),
            Component::RootDir => true,
            Component::Normal(value) => {
                directory::filename_issue(&value.encode_wide().collect::<Vec<_>>()).is_none()
            }
            _ => false,
        })
}

fn read_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES.0)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
}

fn inspect_named(path: &Path) -> io::Result<Proof> {
    let file = read_file(path)?;
    if final_path(&file)?.file_name() != path.file_name() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "The exact name is missing.",
        ));
    }
    Proof::file(&file)
}

fn free_name(
    current: &Path,
    target: &Path,
    roots: [Identity; 2],
    proof: Proof,
) -> io::Result<bool> {
    if compare::path_stamp(target)?.is_none() {
        return Ok(true);
    }
    let spelling_only = roots[0] == roots[1]
        && current.file_name() != target.file_name()
        && compare::ordinal(
            &directory::wide(current.file_name().unwrap()),
            &directory::wide(target.file_name().unwrap()),
        )
        .is_eq();
    // For a case-only rename on an insensitive directory, the old spelling
    // resolves to the current entry. An existing hard link with another
    // spelling must still block undo.
    Ok(spelling_only
        && (|| {
            let target_file = read_file(target).ok()?;
            (final_path(&target_file).ok()?.file_name() == current.file_name()
                && Proof::file(&target_file).ok()? == proof)
                .then_some(())
        })()
        .is_some())
}

#[derive(Clone, PartialEq, Eq)]
pub enum Check {
    Ready,
    Restored,
    CurrentMissing,
    CurrentChanged,
    OriginalOccupied,
    FolderChanged,
    Failed(String),
}

impl Check {
    pub fn label(&self) -> &str {
        match self {
            Self::Ready => "Ready to restore",
            Self::Restored => "Restored",
            Self::CurrentMissing => "Current name is missing",
            Self::CurrentChanged => "Current file changed",
            Self::OriginalOccupied => "Original name is occupied",
            Self::FolderChanged => "A folder changed or is unavailable",
            Self::Failed(error) => error,
        }
    }
}

fn check(receipt: &Receipt) -> Check {
    check_with(receipt, |path| compare::directory_identity(path).ok())
}

fn check_with<'a>(
    receipt: &'a Receipt,
    mut identity: impl FnMut(&'a Path) -> Option<Identity>,
) -> Check {
    if receipt.restored {
        return Check::Restored;
    }
    let from = identity(receipt.from.parent().unwrap());
    let to = identity(receipt.to.parent().unwrap());
    if [from, to] != receipt.roots.map(Some) {
        return Check::FolderChanged;
    }

    match inspect_named(&receipt.to) {
        Ok(proof) if proof == receipt.after => {}
        Ok(_) => return Check::CurrentChanged,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // A completed restore may have reached disk before the history write.
            if inspect_named(&receipt.from).is_ok_and(|proof| proof.same_payload(receipt.before)) {
                return Check::Restored;
            }
            return Check::CurrentMissing;
        }
        Err(error) => return Check::Failed(error.to_string()),
    }

    match free_name(&receipt.to, &receipt.from, receipt.roots, receipt.after) {
        Ok(true) => Check::Ready,
        Ok(false) => Check::OriginalOccupied,
        Err(error) => Check::Failed(error.to_string()),
    }
}

pub fn review(group: &Group) -> Vec<Check> {
    let mut roots = HashMap::new();
    group
        .items
        .iter()
        .map(|receipt| {
            check_with(receipt, |path| {
                *roots
                    .entry(path)
                    .or_insert_with(|| compare::directory_identity(path).ok())
            })
        })
        .collect()
}

fn restore(receipt: &Receipt) -> io::Result<Option<Proof>> {
    let lock_parent = |path: &Path, expected| {
        let lock = OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES.0)
            .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
            .open(path.parent().unwrap())?;
        if compare::directory_handle_identity(&lock)? != expected {
            return Err(io::Error::other("A reviewed folder changed."));
        }
        Ok(lock)
    };
    let to_dir = lock_parent(&receipt.from, receipt.roots[0])?;
    let from_dir = lock_parent(&receipt.to, receipt.roots[1])?;
    let from = final_path(&from_dir)?.join(receipt.to.file_name().unwrap());
    let to = final_path(&to_dir)?.join(receipt.from.file_name().unwrap());
    let file = OpenOptions::new()
        .access_mode((DELETE | FILE_READ_ATTRIBUTES).0)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(&from)?;
    if final_path(&file)?.file_name() != from.file_name() || Proof::file(&file)? != receipt.after {
        return Err(io::Error::other("The current file changed before undo."));
    }
    // The handle prevents another writer or delete while the rename is issued.
    // Windows rejects any intervening destination creation.
    rename_handle(&file, &to)?;
    drop(file);
    // Once the filesystem rename succeeds it is restored even if a racing
    // editor makes the new metadata unavailable for an earlier history entry.
    Ok(inspect_named(&to).ok())
}

pub fn final_path(file: &File) -> io::Result<PathBuf> {
    let handle = HANDLE(file.as_raw_handle());
    let mut path = vec![0; 512];
    loop {
        let length = unsafe { GetFinalPathNameByHandleW(handle, &mut path, FILE_NAME_NORMALIZED) };
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        if (length as usize) < path.len() {
            let path = PathBuf::from(OsString::from_wide(&path[..length as usize]));
            if !path.is_absolute() {
                return Err(io::Error::other("Windows did not return an absolute path."));
            }
            return Ok(path);
        }
        path.resize(length as usize + 1, 0);
    }
}

pub fn rename_handle(file: &File, target: &Path) -> io::Result<()> {
    let destination = extended_path(target);
    let offset = offset_of!(FILE_RENAME_INFO, FileName);
    let length = offset + destination.len() * size_of::<u16>();
    // A zeroed, aligned header sets ReplaceIfExists to false. Use a fully
    // qualified target so Win32 never resolves it against the current directory.
    let mut buffer = vec![0_usize; length.div_ceil(size_of::<usize>())];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    unsafe {
        (*info).FileNameLength = ((destination.len() - 1) * size_of::<u16>()) as u32;
        std::ptr::copy_nonoverlapping(
            destination.as_ptr(),
            info.cast::<u8>().add(offset).cast(),
            destination.len(),
        );
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileRenameInfo,
            info.cast(),
            length as u32,
        )
    }
    .map_err(io::Error::other)
}
