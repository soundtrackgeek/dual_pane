use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use windows::{
    Win32::{
        Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS, FILETIME, SYSTEMTIME},
        Storage::FileSystem::{
            FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
            MOVE_FILE_FLAGS, MoveFileExW, SetFileAttributesW,
        },
        System::Time::{SystemTimeToFileTime, TzSpecificLocalTimeToSystemTime},
    },
    core::PCWSTR,
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, read::ZipFileEntry, write::SimpleFileOptions};

use crate::{
    compare::{self, Identity, Stamp},
    directory::{self, Entry, extended_path, wide},
};

const MAX_DEPTH: usize = 128;
const MAX_PATH_UNITS: usize = 4096;
static STAGING_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub struct Source {
    pub folder: Arc<str>,
    fingerprint: (Identity, Stamp),
}

pub struct Index {
    folders: HashMap<String, Arc<Vec<Entry>>>,
    pub omitted: usize,
    fingerprint: (Identity, Stamp),
}

impl Index {
    pub fn entries(&self, folder: &str) -> Option<Arc<Vec<Entry>>> {
        self.folders.get(folder).cloned()
    }

    pub fn source(&self, folder: Arc<str>) -> Source {
        Source {
            folder,
            fingerprint: self.fingerprint,
        }
    }
}

type Archive = ZipArchive<BufReader<File>>;

fn current(check: &impl Fn() -> bool) -> io::Result<()> {
    if check() {
        Ok(())
    } else {
        Err(io::ErrorKind::Interrupted.into())
    }
}

fn open(path: &Path, check: &impl Fn() -> bool) -> io::Result<(Archive, (Identity, Stamp))> {
    current(check)?;
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)?;
    let fingerprint = compare::regular_handle_identity(&file)?;
    current(check)?;
    let archive =
        ZipArchive::new(BufReader::with_capacity(64 * 1024, file)).map_err(io::Error::other)?;
    if archive.metadata().has_duplicate_names() {
        return Err(io::Error::other(
            "The archive contains duplicate member names.",
        ));
    }
    Ok((archive, fingerprint))
}

pub fn valid_folder(path: &str) -> bool {
    path.is_empty() || valid_member_path(path)
}

fn valid_member_path(path: &str) -> bool {
    if path.is_empty() || path.encode_utf16().count() > MAX_PATH_UNITS {
        return false;
    }
    let mut depth = 0;
    path.split('/').all(|part| {
        depth += 1;
        depth <= MAX_DEPTH
            && directory::filename_issue(&part.encode_utf16().collect::<Vec<_>>()).is_none()
    })
}

fn member_path<'a>(entry: &'a ZipFileEntry<'_>) -> Option<(std::borrow::Cow<'a, str>, bool)> {
    let directory = entry.is_dir();
    let kind = entry.unix_mode().unwrap_or(0) & 0o170000;
    if entry.is_symlink()
        || entry.external_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
        || (kind != 0 && kind != if directory { 0o040000 } else { 0o100000 })
    {
        return None;
    }
    let mut name = entry.name().ok()?;
    if directory && name.ends_with('/') {
        name = match name {
            std::borrow::Cow::Borrowed(value) => {
                std::borrow::Cow::Borrowed(&value[..value.len() - 1])
            }
            std::borrow::Cow::Owned(mut value) => {
                value.pop();
                std::borrow::Cow::Owned(value)
            }
        };
    }
    valid_member_path(&name).then_some((name, directory))
}

fn modified(entry: &ZipFileEntry<'_>) -> FILETIME {
    let Some(date) = entry.last_modified() else {
        return FILETIME::default();
    };
    let local = SYSTEMTIME {
        wYear: date.year(),
        wMonth: u16::from(date.month()),
        wDay: u16::from(date.day()),
        wHour: u16::from(date.hour()),
        wMinute: u16::from(date.minute()),
        wSecond: u16::from(date.second()),
        ..SYSTEMTIME::default()
    };
    let mut utc = SYSTEMTIME::default();
    let mut output = FILETIME::default();
    unsafe {
        if TzSpecificLocalTimeToSystemTime(None, &local, &mut utc).is_ok() {
            let _ = SystemTimeToFileTime(&utc, &mut output);
        }
    }
    output
}

fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

fn folder(folders: &mut HashMap<String, Vec<Entry>>, path: &str, date: FILETIME) {
    if folders.contains_key(path) {
        return;
    }
    let (parent, name) = split(path);
    folder(folders, parent, FILETIME::default());
    folders.get_mut(parent).unwrap().push(Entry {
        name: wide(name).into_boxed_slice(),
        directory: true,
        size: 0,
        modified: date,
        attributes: 0,
    });
    folders.insert(path.into(), Vec::new());
}

pub fn index(path: &Path, check: impl Fn() -> bool) -> io::Result<Index> {
    let (archive, fingerprint) = open(path, &check)?;
    let mut folders = HashMap::from([(String::new(), Vec::new())]);
    let mut omitted = 0;
    for i in 0..archive.len() {
        current(&check)?;
        let entry = archive.by_index_data(i).map_err(io::Error::other)?;
        let Some((path, directory)) = member_path(&entry) else {
            omitted += 1;
            continue;
        };
        if directory {
            folder(&mut folders, &path, modified(&entry));
        } else {
            let (parent, name) = split(&path);
            folder(&mut folders, parent, FILETIME::default());
            folders.get_mut(parent).unwrap().push(Entry {
                name: wide(name).into_boxed_slice(),
                directory: false,
                size: entry.size(),
                modified: modified(&entry),
                attributes: 0,
            });
        }
    }
    drop(archive);
    let folders = folders
        .into_iter()
        .map(|(path, mut entries)| {
            current(&check)?;
            directory::sort(&mut entries);
            Ok((path, Arc::new(entries)))
        })
        .collect::<io::Result<_>>()?;
    Ok(Index {
        folders,
        omitted,
        fingerprint,
    })
}

pub fn read_member(
    path: &Path,
    member: &str,
    limit: usize,
    check: &impl Fn() -> bool,
) -> io::Result<(Vec<u8>, u64)> {
    let (mut archive, _) = open(path, check)?;
    let mut entry = archive.by_name(member).map_err(io::Error::other)?;
    if entry.is_dir()
        || entry.is_symlink()
        || entry.external_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
    {
        return Err(io::Error::other("This ZIP entry cannot be previewed."));
    }
    let size = entry.size();
    let mut bytes = Vec::with_capacity(limit.min(size as usize));
    let mut buffer = [0_u8; 32 * 1024];
    while bytes.len() < limit {
        current(check)?;
        let wanted = (limit - bytes.len()).min(buffer.len());
        match entry.read(&mut buffer[..wanted]) {
            Ok(0) => break,
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok((bytes, size))
}

struct PackEntry {
    source: PathBuf,
    archive_name: String,
    directory: bool,
    size: u64,
}

pub struct Packer {
    writer: Option<ZipWriter<BufWriter<File>>>,
    stage: Stage,
    _root: File,
    staged_file: PathBuf,
    target: PathBuf,
    groups: Vec<Vec<PackEntry>>,
    buffer: Vec<u8>,
    pub total: u128,
}

impl Packer {
    pub fn prepare(
        source_dir: &Path,
        destination_dir: &Path,
        archive_name: &OsStr,
        names: &[OsString],
        check: impl Fn() -> bool,
    ) -> io::Result<Self> {
        current(&check)?;
        let root = lock_directory(destination_dir)?;
        let destination = fs::canonicalize(destination_dir)?;
        if compare::directory_identity(&destination)? != compare::directory_handle_identity(&root)?
        {
            return Err(io::Error::other("The destination folder changed."));
        }
        let target = destination.join(archive_name);
        match fs::symlink_metadata(&target) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "The destination archive already exists.",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        let mut groups = Vec::with_capacity(names.len());
        let mut total = 0_u128;
        for name in names {
            current(&check)?;
            let mut entries = Vec::new();
            let full = source_dir.join(name);
            let top = name
                .to_str()
                .ok_or_else(|| io::Error::other("An item name is not valid UTF-8 for ZIP."))?;
            collect_pack_entries(&full, top, &mut entries, &mut total, &check)?;
            groups.push(entries);
        }

        let stage = Stage::create(&destination)?;
        let staged_file = stage.path.join("archive.zip");
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged_file)?;
        let writer = ZipWriter::new(BufWriter::with_capacity(256 * 1024, file));
        Ok(Self {
            writer: Some(writer),
            stage,
            _root: root,
            staged_file,
            target,
            groups,
            buffer: vec![0; 256 * 1024],
            total,
        })
    }

    pub fn item(
        &mut self,
        index: usize,
        check: impl Fn() -> bool,
        mut progress: impl FnMut(usize),
    ) -> io::Result<()> {
        current(&check)?;
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| io::Error::other("The archive writer is already closed."))?;
        for entry in &self.groups[index] {
            current(&check)?;
            if entry.directory {
                let options =
                    SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
                writer
                    .add_directory(&entry.archive_name, options)
                    .map_err(io::Error::other)?;
                continue;
            }
            let options = SimpleFileOptions::default()
                .compression_method(CompressionMethod::Deflated)
                .large_file(entry.size > u32::MAX as u64);
            writer
                .start_file(&entry.archive_name, options)
                .map_err(io::Error::other)?;
            let mut input = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
                .open(&entry.source)?;
            loop {
                current(&check)?;
                let count = input.read(&mut self.buffer)?;
                if count == 0 {
                    break;
                }
                writer.write_all(&self.buffer[..count])?;
                progress(count);
            }
        }
        Ok(())
    }

    pub fn finish(mut self, check: impl Fn() -> bool) -> io::Result<()> {
        current(&check)?;
        if let Some(writer) = self.writer.take() {
            let mut file = writer.finish().map_err(io::Error::other)?;
            file.flush()?;
        }
        current(&check)?;
        let from = extended_path(&self.staged_file);
        let to = extended_path(&self.target);
        unsafe {
            MoveFileExW(
                PCWSTR(from.as_ptr()),
                PCWSTR(to.as_ptr()),
                MOVE_FILE_FLAGS(0),
            )
        }
        .map_err(|error| {
            if error.code() == ERROR_ALREADY_EXISTS.to_hresult()
                || error.code() == ERROR_FILE_EXISTS.to_hresult()
            {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "The destination archive already exists.",
                )
            } else {
                io::Error::other(error)
            }
        })?;
        drop(self.stage);
        Ok(())
    }
}

fn collect_pack_entries(
    path: &Path,
    relative: &str,
    out: &mut Vec<PackEntry>,
    total: &mut u128,
    check: &impl Fn() -> bool,
) -> io::Result<()> {
    current(check)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Ok(());
    }
    if metadata.is_dir() {
        let mut children = Vec::new();
        for entry in fs::read_dir(path)? {
            current(check)?;
            let entry = entry?;
            children.push(entry.file_name());
        }
        if children.is_empty() {
            out.push(PackEntry {
                source: path.to_path_buf(),
                archive_name: format!("{relative}/"),
                directory: true,
                size: 0,
            });
            return Ok(());
        }
        children.sort();
        for child in children {
            let Some(child_str) = child.to_str() else {
                return Err(io::Error::other("A file name is not valid UTF-8 for ZIP."));
            };
            let next_rel = format!("{relative}/{child_str}");
            collect_pack_entries(&path.join(&child), &next_rel, out, total, check)?;
        }
    } else if metadata.is_file() {
        let size = metadata.len();
        *total += u128::from(size);
        out.push(PackEntry {
            source: path.to_path_buf(),
            archive_name: relative.to_owned(),
            directory: false,
            size,
        });
    }
    Ok(())
}

struct Member {
    index: usize,
    relative: Box<str>,
    directory: bool,
    size: u64,
}

pub struct Extractor {
    archive: Archive,
    _root: File,
    destination: PathBuf,
    groups: Vec<Vec<Member>>,
    buffer: Vec<u8>,
    pub total: u128,
}

impl Extractor {
    pub fn prepare<'a>(
        path: &Path,
        source: &Source,
        destination: &Path,
        names: impl ExactSizeIterator<Item = &'a OsStr>,
        check: impl Fn() -> bool,
    ) -> io::Result<Self> {
        let (archive, fingerprint) = open(path, &check)?;
        if fingerprint != source.fingerprint {
            return Err(io::Error::other(
                "The ZIP changed. Refresh it and select the members again.",
            ));
        }
        let root = lock_directory(destination)?;
        let destination = fs::canonicalize(destination)?;
        if compare::directory_identity(&destination)? != compare::directory_handle_identity(&root)?
        {
            return Err(io::Error::other("The destination folder changed."));
        }

        let mut groups = (0..names.len()).map(|_| Vec::new()).collect::<Vec<_>>();
        let mut selected = HashMap::new();
        for (index, name) in names.enumerate() {
            let name = name
                .to_str()
                .filter(|name| valid_member_path(name) && !name.contains('/'))
                .ok_or_else(|| io::Error::other("An invalid archive name was selected."))?;
            if selected.insert(name, index).is_some() {
                return Err(io::Error::other("The selected ZIP names are ambiguous."));
            }
        }
        let prefix = if source.folder.is_empty() {
            String::new()
        } else {
            format!("{}/", source.folder)
        };
        let mut total = 0;
        for i in 0..archive.len() {
            current(&check)?;
            let entry = archive.by_index_data(i).map_err(io::Error::other)?;
            let Some((path, directory)) = member_path(&entry) else {
                continue;
            };
            let Some(path) = path.strip_prefix(&prefix) else {
                continue;
            };
            let (first, relative) = path.split_once('/').unwrap_or((path, ""));
            if let Some(&index) = selected.get(first) {
                let size = if directory { 0 } else { entry.size() };
                total += u128::from(size);
                groups[index].push(Member {
                    index: i,
                    relative: relative.into(),
                    directory,
                    size,
                });
            }
        }
        Ok(Self {
            archive,
            _root: root,
            destination,
            groups,
            buffer: vec![0; 256 * 1024],
            total,
        })
    }

    pub fn item(
        &mut self,
        index: usize,
        name: &OsStr,
        check: impl Fn() -> bool,
        mut progress: impl FnMut(usize),
    ) -> io::Result<()> {
        current(&check)?;
        let members = &self.groups[index];
        if members.is_empty() {
            return Err(io::Error::other("The selected ZIP member was not found."));
        }
        let directory = members
            .iter()
            .any(|member| member.directory || !member.relative.is_empty());
        if directory
            && members
                .iter()
                .any(|member| !member.directory && member.relative.is_empty())
        {
            return Err(io::Error::other(
                "The same ZIP name is used for a file and a folder.",
            ));
        }
        let target = self.destination.join(name);
        match fs::symlink_metadata(&target) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "The destination already exists.",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let stage = Stage::create(&self.destination)?;
        let output = stage.path.join("payload");
        if directory {
            fs::create_dir(&output)?;
            let payload = lock_directory(&output)?;
            let mut created = HashSet::new();
            for member in members {
                current(&check)?;
                if member.relative.is_empty() {
                    continue;
                }
                let (parent, filename) = split(&member.relative);
                let folders = if member.directory {
                    member.relative.as_ref()
                } else {
                    parent
                };
                let (path, _locks) = build_directories(&output, folders, &mut created)?;
                if !member.directory {
                    write_member(
                        &mut self.archive,
                        member,
                        &path.join(filename),
                        &mut self.buffer,
                        &check,
                        &mut progress,
                    )?;
                }
            }
            drop(payload);
        } else {
            if members.len() != 1 {
                return Err(io::Error::other("The ZIP contains ambiguous file names."));
            }
            write_member(
                &mut self.archive,
                &members[0],
                &output,
                &mut self.buffer,
                &check,
                &mut progress,
            )?;
        }
        current(&check)?;
        let from = extended_path(&output);
        let to = extended_path(&target);
        // Flags zero is a same-volume rename that fails if the target exists.
        unsafe {
            MoveFileExW(
                PCWSTR(from.as_ptr()),
                PCWSTR(to.as_ptr()),
                MOVE_FILE_FLAGS(0),
            )
        }
        .map_err(|error| {
            if error.code() == ERROR_ALREADY_EXISTS.to_hresult()
                || error.code() == ERROR_FILE_EXISTS.to_hresult()
            {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "The destination already exists.",
                )
            } else {
                io::Error::other(error)
            }
        })
    }
}

fn write_member(
    archive: &mut Archive,
    member: &Member,
    target: &Path,
    buffer: &mut [u8],
    check: &impl Fn() -> bool,
    progress: &mut impl FnMut(usize),
) -> io::Result<()> {
    let mut reader = archive.by_index(member.index).map_err(io::Error::other)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(reject_alias)?;
    let mut bytes = 0_u64;
    loop {
        current(check)?;
        let count = reader.read(buffer)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .filter(|&bytes| bytes <= member.size)
            .ok_or_else(|| io::Error::other("The ZIP member exceeds its recorded size."))?;
        output.write_all(&buffer[..count])?;
        progress(count);
    }
    if bytes != member.size {
        return Err(io::Error::other(
            "The ZIP member did not match its recorded size.",
        ));
    }
    output.flush()
}

fn lock_directory(path: &Path) -> io::Result<File> {
    let directory = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES.0)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)?;
    let metadata = directory.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(io::Error::other(
            "Extraction requires an ordinary directory.",
        ));
    }
    Ok(directory)
}

fn reject_alias(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::AlreadyExists {
        io::Error::other("Multiple ZIP members resolve to the same Windows name.")
    } else {
        error
    }
}

fn build_directories(
    root: &Path,
    folders: &str,
    created: &mut HashSet<PathBuf>,
) -> io::Result<(PathBuf, Vec<File>)> {
    let mut path = root.to_path_buf();
    let mut locks = Vec::new();
    for component in folders.split('/').filter(|component| !component.is_empty()) {
        path.push(component);
        if !created.contains(&path) {
            fs::create_dir(&path).map_err(reject_alias)?;
            created.insert(path.clone());
        }
        locks.push(lock_directory(&path)?);
    }
    Ok((path, locks))
}

struct Stage {
    path: PathBuf,
    lock: Option<File>,
}

impl Stage {
    fn create(root: &Path) -> io::Result<Self> {
        for _ in 0..64 {
            let id = STAGING_ID.fetch_add(1, Ordering::Relaxed);
            let path = root.join(format!(".dual-pane-extract-{}-{id}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => {
                    let hidden = extended_path(&path);
                    unsafe {
                        let _ = SetFileAttributesW(PCWSTR(hidden.as_ptr()), FILE_ATTRIBUTE_HIDDEN);
                    }
                    let mut stage = Self { path, lock: None };
                    stage.lock = Some(lock_directory(&stage.path)?);
                    return Ok(stage);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other(
            "Could not create an extraction staging folder.",
        ))
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        self.lock.take();
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn packs_files_and_folders_and_reads_bounded_member_preview() -> io::Result<()> {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "filehelm-pack-test-{}-{unique}",
            std::process::id()
        ));
        let source = root.join("src");
        let dest = root.join("dst");
        fs::create_dir_all(source.join("nested"))?;
        fs::create_dir_all(source.join("empty_dir"))?;
        fs::create_dir_all(&dest)?;
        fs::write(source.join("hello.txt"), b"Hello from Filehelm ZIP!")?;
        fs::write(source.join("nested").join("data.txt"), b"0123456789abcdef")?;

        let names = vec![
            OsString::from("hello.txt"),
            OsString::from("nested"),
            OsString::from("empty_dir"),
        ];
        let mut packer =
            Packer::prepare(&source, &dest, OsStr::new("bundle.zip"), &names, || true)?;
        let mut progress = 0_usize;
        for i in 0..names.len() {
            packer.item(i, || true, |n| progress += n)?;
        }
        assert_eq!(progress as u128, packer.total);
        packer.finish(|| true)?;

        let archive = dest.join("bundle.zip");
        let idx = index(&archive, || true)?;
        assert_eq!(idx.omitted, 0);
        assert!(idx.entries("").unwrap().len() == 3);
        assert!(idx.entries("nested").unwrap().len() == 1);
        assert!(idx.entries("empty_dir").unwrap().is_empty());

        let (head, total) = read_member(&archive, "nested/data.txt", 6, &|| true)?;
        assert_eq!(head, b"012345");
        assert_eq!(total, 16);

        let existing = Packer::prepare(&source, &dest, OsStr::new("bundle.zip"), &names, || true);
        assert!(matches!(existing, Err(error) if error.kind() == io::ErrorKind::AlreadyExists));

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn extraction_omits_unsafe_names_and_protects_existing_targets() -> io::Result<()> {
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let root = std::env::temp_dir().join(format!(
            "dual-pane-zip-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root)?;
        let fixture = Fixture(root);
        let destination = fixture.0.join("out");
        fs::create_dir(&destination)?;
        let source = fixture.0.join("example.zip");
        let mut zip = ZipWriter::new(File::create(&source)?);
        for (name, data) in [
            ("album/á.txt", "music"),
            ("keep.txt", "new"),
            ("../outside.txt", "outside"),
        ] {
            zip.start_file(name, SimpleFileOptions::default())
                .map_err(io::Error::other)?;
            zip.write_all(data.as_bytes())?;
        }
        drop(zip.finish().map_err(io::Error::other)?);
        fs::write(destination.join("keep.txt"), b"original")?;

        let index = index(&source, || true)?;
        assert_eq!(index.omitted, 1);
        assert_eq!(index.entries("").unwrap().len(), 2);
        let names = [OsStr::new("album"), OsStr::new("keep.txt")];
        let mut extract = Extractor::prepare(
            &source,
            &index.source(Arc::from("")),
            &destination,
            names.into_iter(),
            || true,
        )?;
        extract.item(0, names[0], || true, |_| {})?;
        assert_eq!(fs::read(destination.join("album").join("á.txt"))?, b"music");
        assert_eq!(
            extract
                .item(1, names[1], || true, |_| {})
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(destination.join("keep.txt"))?, b"original");
        assert!(!fixture.0.join("outside.txt").exists());
        assert_eq!(fs::read_dir(destination)?.count(), 2);
        Ok(())
    }
}
