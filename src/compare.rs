use std::{
    cmp::Ordering,
    ffi::OsString,
    fs::{File, OpenOptions},
    io::{self, Read},
    os::windows::fs::OpenOptionsExt,
    os::windows::io::AsRawHandle,
    path::{Path, PathBuf},
    sync::Arc,
};

use windows::Win32::{
    Foundation::HANDLE,
    Globalization::{CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN, CompareStringOrdinal},
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DEVICE, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
    },
};

use crate::directory::{self, Entry};

const ABSENT: u32 = u32::MAX;
const CHUNK: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Difference {
    LeftOnly,
    RightOnly,
    LeftNewer,
    RightNewer,
    SameMetadata,
    DifferentSize,
    TimeUnavailable,
    BothFolders,
    TypeConflict,
    Ambiguous,
}

impl Difference {
    pub const ALL: [Self; 10] = [
        Self::LeftOnly,
        Self::RightOnly,
        Self::LeftNewer,
        Self::RightNewer,
        Self::SameMetadata,
        Self::DifferentSize,
        Self::TimeUnavailable,
        Self::BothFolders,
        Self::TypeConflict,
        Self::Ambiguous,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::LeftOnly => "Left only",
            Self::RightOnly => "Right only",
            Self::LeftNewer => "Newer on left",
            Self::RightNewer => "Newer on right",
            Self::SameMetadata => "Same size and time",
            Self::DifferentSize => "Different sizes",
            Self::TimeUnavailable => "Time unavailable",
            Self::BothFolders => "Folders on both sides",
            Self::TypeConflict => "File/folder conflict",
            Self::Ambiguous => "Ambiguous name",
        }
    }
}

#[derive(Clone, Copy)]
pub struct Row {
    items: [u32; 2],
    pub difference: Difference,
    directions: u8,
}

pub struct Comparison {
    pub paths: [PathBuf; 2],
    entries: [Arc<Vec<Entry>>; 2],
    pub rows: Vec<Row>,
}

impl Comparison {
    pub fn build(
        paths: [PathBuf; 2],
        entries: [Arc<Vec<Entry>>; 2],
        current: impl Fn() -> bool,
    ) -> io::Result<Self> {
        let left = Index::new(&entries[0], &current)?;
        let right = Index::new(&entries[1], &current)?;
        let mut rows = Vec::with_capacity(left.order.len().max(right.order.len()));
        let (mut l, mut r) = (0, 0);
        while l < left.order.len() || r < right.order.len() {
            if rows.len().is_multiple_of(1024) {
                check(&current)?;
            }
            let order = match (left.at(l), right.at(r)) {
                (Some(a), Some(b)) => ordinal(a, b),
                (Some(_), None) => Ordering::Less,
                _ => Ordering::Greater,
            };
            let count_l = if order == Ordering::Greater {
                0
            } else {
                left.group(l)
            };
            let count_r = if order == Ordering::Less {
                0
            } else {
                right.group(r)
            };
            if count_l > 1 || count_r > 1 {
                for index in l..l + count_l {
                    rows.push(Row {
                        items: [left.order[index], ABSENT],
                        difference: Difference::Ambiguous,
                        directions: 0,
                    });
                }
                for index in r..r + count_r {
                    rows.push(Row {
                        items: [ABSENT, right.order[index]],
                        difference: Difference::Ambiguous,
                        directions: 0,
                    });
                }
            } else {
                let items = [
                    if count_l == 0 { ABSENT } else { left.order[l] },
                    if count_r == 0 { ABSENT } else { right.order[r] },
                ];
                let [a, b] = [left.entry(items[0]), right.entry(items[1])];
                let directions = match (a, b) {
                    (Some(a), Some(b)) if regular(a) && regular(b) => 3,
                    (Some(a), None) if regular(a) => 1,
                    (None, Some(b)) if regular(b) => 2,
                    _ => 0,
                };
                rows.push(Row {
                    items,
                    difference: classify(a, b),
                    directions,
                });
            }
            l += count_l;
            r += count_r;
        }
        check(&current)?;
        Ok(Self {
            paths,
            entries,
            rows,
        })
    }

    pub fn refresh(paths: [PathBuf; 2], current: impl Fn() -> bool) -> io::Result<Self> {
        let entries = load(&paths, &current)?.map(Arc::new);
        Self::build(paths, entries, current)
    }

    pub fn entry(&self, row: usize, side: usize) -> Option<&Entry> {
        self.entries[side].get(self.rows[row].items[side] as usize)
    }

    pub fn name(&self, row: usize) -> &[u16] {
        &self
            .entry(row, 0)
            .or_else(|| self.entry(row, 1))
            .unwrap()
            .name
    }

    pub fn eligible(&self, row: usize, source: usize) -> bool {
        self.rows[row].directions & (1 << source) != 0
    }

    pub fn eligible_either_way(&self, row: usize) -> bool {
        self.rows[row].directions != 0
    }

    pub fn contents(
        &self,
        rows: &[usize],
        current: impl Fn() -> bool,
    ) -> io::Result<Vec<(usize, Exact)>> {
        let mut buffers = [vec![0; CHUNK], vec![0; CHUNK]];
        let mut results = Vec::with_capacity(rows.len());
        for &row in rows {
            check(&current)?;
            let entries = [self.entry(row, 0), self.entry(row, 1)];
            if let [Some(left), Some(right)] = entries
                && self.rows[row].directions == 3
            {
                let exact = compare_bytes(&self.paths, [left, right], &mut buffers, &current)
                    .unwrap_or(Exact::Unavailable);
                results.push((row, exact));
            }
        }
        check(&current)?;
        Ok(results)
    }

    pub fn validate(&self, plan: &Plan, current: impl Fn() -> bool) -> io::Result<Vec<usize>> {
        self.check_roots(plan)?;
        let fresh = load(&self.paths, &current)?;
        let indexes = [
            Index::new(&fresh[0], &current)?,
            Index::new(&fresh[1], &current)?,
        ];
        let mut changed = Vec::new();
        for (position, &row) in plan.rows.iter().enumerate() {
            if position.is_multiple_of(1024) {
                check(&current)?;
            }
            let key = self.name(row);
            for (side, index) in indexes.iter().enumerate() {
                let found = index.unique(key);
                if !matches!((self.entry(row, side), found), (None, Ok(None)))
                    && !matches!((self.entry(row, side), found),
                    (Some(before), Ok(Some(after))) if same_entry(before, after))
                {
                    changed.push(row);
                    break;
                }
            }
        }
        check(&current)?;
        self.check_roots(plan)?;
        Ok(changed)
    }

    pub fn prepare(&self, source: usize, rows: Vec<usize>) -> io::Result<Plan> {
        if rows.is_empty() || rows.iter().any(|&row| !self.eligible(row, source)) {
            return Err(io::Error::other(
                "Select regular files present on the source side with an unambiguous destination.",
            ));
        }
        let roots = self.identities()?;
        if roots[0] == roots[1] {
            return Err(io::Error::other("Both paths resolve to the same folder."));
        }
        Ok(Plan {
            source,
            rows,
            roots,
        })
    }

    pub fn check_roots(&self, plan: &Plan) -> io::Result<()> {
        if self.identities()? != plan.roots {
            return Err(io::Error::other(
                "A reviewed path now points to a different folder.",
            ));
        }
        Ok(())
    }

    fn identities(&self) -> io::Result<[Identity; 2]> {
        Ok([
            directory_identity(&self.paths[0])?,
            directory_identity(&self.paths[1])?,
        ])
    }

    pub fn copy_items<'a>(&'a self, plan: &'a Plan) -> impl Iterator<Item = CopyItem> + 'a {
        let source = plan.source;
        plan.rows.iter().map(move |&row| {
            let file = self.entry(row, source).unwrap();
            let target = self.entry(row, 1 - source);
            CopyItem {
                source: self.paths[source].join(file.filename()),
                destination_name: target.unwrap_or(file).filename(),
                original: Stamp::entry(file),
                replacement: target.map(Stamp::entry),
            }
        })
    }
}

impl Plan {
    pub(crate) fn oriented_roots(&self) -> [Identity; 2] {
        [self.roots[self.source], self.roots[1 - self.source]]
    }
}

#[derive(Clone)]
pub struct Plan {
    pub source: usize,
    pub rows: Vec<usize>,
    roots: [Identity; 2],
}

pub(crate) type Identity = (u32, u32, u32);

pub struct CopyItem {
    pub source: PathBuf,
    pub destination_name: OsString,
    pub(crate) original: Stamp,
    pub(crate) replacement: Option<Stamp>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Exact {
    Equal,
    Different,
    Changed,
    Unavailable,
}

impl Exact {
    pub fn label(self) -> &'static str {
        match self {
            Self::Equal => "Contents equal",
            Self::Different => "Contents differ",
            Self::Changed => "File changed",
            Self::Unavailable => "Could not read",
        }
    }
}

pub(crate) struct Index<'a> {
    entries: &'a [Entry],
    order: Vec<u32>,
}

impl<'a> Index<'a> {
    pub(crate) fn new(entries: &'a [Entry], current: impl Fn() -> bool) -> io::Result<Self> {
        check(&current)?;
        let len = u32::try_from(entries.len()).map_err(io::Error::other)?;
        let mut order = (0..len).collect::<Vec<_>>();
        order.sort_unstable_by(|&left, &right| {
            ordinal(&entries[left as usize].name, &entries[right as usize].name)
                .then_with(|| left.cmp(&right))
        });
        check(&current)?;
        Ok(Self { entries, order })
    }

    fn entry(&self, index: u32) -> Option<&'a Entry> {
        self.entries.get(index as usize)
    }

    fn at(&self, index: usize) -> Option<&'a [u16]> {
        self.order
            .get(index)
            .map(|&row| self.entries[row as usize].name.as_ref())
    }

    fn group(&self, start: usize) -> usize {
        let name = self.at(start).unwrap();
        1 + (start + 1..self.order.len())
            .take_while(|&index| ordinal(name, self.at(index).unwrap()).is_eq())
            .count()
    }

    pub(crate) fn unique(&self, key: &[u16]) -> Result<Option<&'a Entry>, ()> {
        let position = self
            .order
            .partition_point(|&index| ordinal(&self.entries[index as usize].name, key).is_lt());
        if !self
            .at(position)
            .is_some_and(|name| ordinal(name, key).is_eq())
        {
            return Ok(None);
        }
        if self
            .at(position + 1)
            .is_some_and(|name| ordinal(name, key).is_eq())
        {
            return Err(());
        }
        Ok(self.entry(self.order[position]))
    }
}

pub(crate) fn ordinal(left: &[u16], right: &[u16]) -> Ordering {
    let result = unsafe { CompareStringOrdinal(left, right, true) };
    if result == CSTR_EQUAL {
        Ordering::Equal
    } else if result == CSTR_LESS_THAN {
        Ordering::Less
    } else if result == CSTR_GREATER_THAN {
        Ordering::Greater
    } else {
        left.cmp(right)
    }
}

fn classify(left: Option<&Entry>, right: Option<&Entry>) -> Difference {
    let (left, right) = match (left, right) {
        (Some(left), Some(right)) => (left, right),
        (Some(_), None) => return Difference::LeftOnly,
        _ => return Difference::RightOnly,
    };
    if left.directory != right.directory {
        return Difference::TypeConflict;
    }
    if left.directory {
        return Difference::BothFolders;
    }
    let (a, b) = (left.write_time(), right.write_time());
    if a == 0 || b == 0 {
        return Difference::TimeUnavailable;
    }
    match a.cmp(&b) {
        Ordering::Less => Difference::RightNewer,
        Ordering::Greater => Difference::LeftNewer,
        Ordering::Equal if left.size != right.size => Difference::DifferentSize,
        Ordering::Equal => Difference::SameMetadata,
    }
}

fn regular(entry: &Entry) -> bool {
    !entry.directory
        && entry.attributes & (FILE_ATTRIBUTE_REPARSE_POINT.0 | FILE_ATTRIBUTE_DEVICE.0) == 0
        && directory::filename_issue(&entry.name[..entry.name.len() - 1]).is_none()
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stamp {
    size: u64,
    time: u64,
    attributes: u32,
}

impl Stamp {
    pub(crate) fn entry(entry: &Entry) -> Self {
        Self {
            size: entry.size,
            time: entry.write_time(),
            attributes: entry.attributes,
        }
    }

    pub(crate) fn regular(self) -> bool {
        self.attributes
            & (FILE_ATTRIBUTE_DIRECTORY.0
                | FILE_ATTRIBUTE_REPARSE_POINT.0
                | FILE_ATTRIBUTE_DEVICE.0)
            == 0
    }

    fn handle(info: &BY_HANDLE_FILE_INFORMATION) -> Self {
        Self {
            size: u64::from(info.nFileSizeHigh) << 32 | u64::from(info.nFileSizeLow),
            time: u64::from(info.ftLastWriteTime.dwHighDateTime) << 32
                | u64::from(info.ftLastWriteTime.dwLowDateTime),
            attributes: info.dwFileAttributes,
        }
    }
}

pub(crate) fn path_stamp(path: &Path) -> io::Result<Option<Stamp>> {
    use std::os::windows::fs::MetadataExt;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(Stamp {
            size: metadata.file_size(),
            time: metadata.last_write_time(),
            attributes: metadata.file_attributes(),
        })),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn same_entry(before: &Entry, after: &Entry) -> bool {
    before.name == after.name && Stamp::entry(before) == Stamp::entry(after)
}

fn load(paths: &[PathBuf; 2], current: impl Fn() -> bool) -> io::Result<[Vec<Entry>; 2]> {
    let read = |path| {
        check(&current)?;
        directory::read(path, &current, |_| {}).map_err(io::Error::other)
    };
    Ok([read(&paths[0])?, read(&paths[1])?])
}

fn file_info(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .map_err(io::Error::other)?;
    Ok(info)
}

pub(crate) fn directory_identity(path: &Path) -> io::Result<Identity> {
    let file = OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0)
        .open(path)?;
    directory_handle_identity(&file)
}

pub(crate) fn directory_handle_identity(file: &File) -> io::Result<Identity> {
    let info = file_info(file)?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 == 0 {
        return Err(io::Error::other(
            "A reviewed folder is no longer a directory.",
        ));
    }
    Ok((
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
    ))
}

pub(crate) fn regular_identity(path: &Path) -> io::Result<(Identity, Stamp)> {
    let file = OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0)
        .open(path)?;
    regular_handle_identity(&file)
}

pub(crate) fn regular_handle_identity(file: &File) -> io::Result<(Identity, Stamp)> {
    let info = file_info(file)?;
    let stamp = Stamp::handle(&info);
    if !stamp.regular() || (info.nFileIndexHigh == 0 && info.nFileIndexLow == 0) {
        return Err(io::Error::other("A stable file identity is unavailable."));
    }
    Ok((
        (
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ),
        stamp,
    ))
}

pub(crate) fn verify_review<'a>(
    paths: &[PathBuf; 2],
    files: impl IntoIterator<
        Item = (
            &'a std::ffi::OsStr,
            &'a std::ffi::OsStr,
            Stamp,
            Option<Stamp>,
        ),
    >,
    current: impl Fn() -> bool,
) -> io::Result<()> {
    let fresh = load(paths, &current)?;
    let indexes = [
        Index::new(&fresh[0], &current)?,
        Index::new(&fresh[1], &current)?,
    ];
    for (source, destination, original, replacement) in files {
        check(&current)?;
        for (side, (name, expected)) in [(source, Some(original)), (destination, replacement)]
            .into_iter()
            .enumerate()
        {
            let wide = directory::wide(name);
            let matches = match (indexes[side].unique(&wide), expected) {
                (Ok(None), None) => true,
                (Ok(Some(entry)), Some(stamp)) => {
                    entry.name.as_ref() == wide && Stamp::entry(entry) == stamp
                }
                _ => false,
            };
            if !matches {
                return Err(io::Error::other(format!(
                    "A reviewed item changed. Compare the folders and review the copy again.\n{}",
                    paths[side].join(name).display(),
                )));
            }
        }
    }
    Ok(())
}

fn compare_bytes(
    paths: &[PathBuf; 2],
    entries: [&Entry; 2],
    buffers: &mut [Vec<u8>; 2],
    current: impl Fn() -> bool,
) -> io::Result<Exact> {
    let open = |side: usize| {
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(paths[side].join(entries[side].filename()))
    };
    let mut files = [open(0)?, open(1)?];
    let (left, right) = (entries[0], entries[1]);
    for side in 0..2 {
        let info = file_info(&files[side])?;
        if Stamp::handle(&info) != Stamp::entry(entries[side]) {
            return Ok(Exact::Changed);
        }
    }
    if left.size != right.size {
        return Ok(Exact::Different);
    }
    let mut remaining = left.size;
    while remaining > 0 {
        check(&current)?;
        let length = remaining.min(CHUNK as u64) as usize;
        for side in 0..2 {
            files[side].read_exact(&mut buffers[side][..length])?;
        }
        if buffers[0][..length] != buffers[1][..length] {
            return Ok(Exact::Different);
        }
        remaining -= length as u64;
    }
    check(&current)?;
    Ok(Exact::Equal)
}

fn check(current: impl Fn() -> bool) -> io::Result<()> {
    if current() {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::Interrupted, "Canceled"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::wide;
    use windows::Win32::Foundation::FILETIME;

    fn entry(name: &str, size: u64, time: u32, directory: bool) -> Entry {
        Entry {
            name: wide(name).into_boxed_slice(),
            directory,
            size,
            modified: FILETIME {
                dwLowDateTime: time,
                dwHighDateTime: 0,
            },
            attributes: if directory {
                FILE_ATTRIBUTE_DIRECTORY.0
            } else {
                0
            },
        }
    }

    #[test]
    fn groups_case_variants_safely_and_reports_file_and_folder_differences() {
        let paths = [PathBuf::from(r"C:\Left"), PathBuf::from(r"C:\Right")];
        let entries = [
            Arc::new(vec![
                entry("same.TXT", 10, 12, false),
                entry("Newer", 8, 14, false),
                entry("Conflict", 0, 0, true),
                entry("Left", 3, 6, false),
                entry("A", 1, 2, false),
                entry("a", 1, 2, false),
            ]),
            Arc::new(vec![
                entry("same.txt", 10, 12, false),
                entry("Newer", 8, 10, false),
                entry("Conflict", 2, 0, false),
                entry("Right", 3, 6, false),
                entry("A", 1, 2, false),
            ]),
        ];
        let result = Comparison::build(paths, entries, || true).unwrap();
        assert_eq!(
            result
                .rows
                .iter()
                .map(|row| row.difference)
                .collect::<Vec<_>>(),
            [
                Difference::Ambiguous,
                Difference::Ambiguous,
                Difference::Ambiguous,
                Difference::TypeConflict,
                Difference::LeftOnly,
                Difference::LeftNewer,
                Difference::RightOnly,
                Difference::SameMetadata,
            ],
        );
        assert!(!result.eligible(0, 0));
        assert!(!result.eligible(3, 0));
        assert!(result.eligible(4, 0));
        let plan = Plan {
            source: 0,
            rows: vec![7],
            roots: [(0, 0, 0); 2],
        };
        assert_eq!(
            result.copy_items(&plan).next().unwrap().destination_name,
            "same.txt"
        );
    }

    struct Folders(PathBuf);

    impl Folders {
        fn create() -> Self {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap();
            let root = std::env::temp_dir().join(format!(
                "dual-pane-compare-{}-{}",
                std::process::id(),
                now.as_nanos()
            ));
            std::fs::create_dir(&root).unwrap();
            let fixture = Self(root);
            for name in ["left", "right"] {
                std::fs::create_dir(fixture.0.join(name)).unwrap();
            }
            fixture
        }
    }

    impl Drop for Folders {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn compares_full_contents_in_chunks_and_flags_changes_before_copy() {
        let fixture = Folders::create();
        let paths = [fixture.0.join("left"), fixture.0.join("right")];
        let left_file = paths[0].join("test.txt");
        let right_file = paths[1].join("test.txt");
        let mut data = vec![11; CHUNK + 2];
        std::fs::write(&left_file, &data).unwrap();
        data[CHUNK + 1] = 99;
        std::fs::write(&right_file, data).unwrap();
        let modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        for path in [&left_file, &right_file] {
            OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(modified))
                .unwrap();
        }

        let comparison = Comparison::refresh(paths.clone(), || true).unwrap();
        assert_eq!(comparison.rows[0].difference, Difference::SameMetadata);
        assert!(comparison.contents(&[0], || false).is_err());
        assert!(matches!(
            comparison.contents(&[0], || true).unwrap().as_slice(),
            [(0, Exact::Different)]
        ));
        let plan = comparison.prepare(0, vec![0]).unwrap();
        assert!(comparison.validate(&plan, || true).unwrap().is_empty());
        let copy = comparison.copy_items(&plan).next().unwrap();
        let verify = || {
            verify_review(
                &paths,
                [(
                    copy.source.file_name().unwrap(),
                    copy.destination_name.as_os_str(),
                    copy.original,
                    copy.replacement,
                )],
                || true,
            )
        };
        assert!(verify().is_ok());

        std::fs::write(&right_file, b"changed since review").unwrap();
        assert_eq!(comparison.validate(&plan, || true).unwrap(), vec![0]);
        assert!(verify().is_err());
        let retired = fixture.0.join("retired");
        std::fs::rename(&paths[1], retired).unwrap();
        std::fs::create_dir(&paths[1]).unwrap();
        assert!(comparison.validate(&plan, || true).is_err());
    }
}
