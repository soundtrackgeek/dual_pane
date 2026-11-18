use std::{
    ffi::{OsStr, OsString},
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

use windows::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_FILES, FILETIME, HANDLE, SYSTEMTIME},
        Globalization::{FIND_FROMSTART, FindStringOrdinal},
        Storage::FileSystem::{
            FILE_ATTRIBUTE_DIRECTORY, FIND_FIRST_EX_LARGE_FETCH, FindClose, FindExInfoBasic,
            FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, WIN32_FIND_DATAW,
        },
        System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTimeEx},
        UI::Shell::StrCmpLogicalW,
    },
    core::PCWSTR,
};

pub fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value
        .as_ref()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[derive(Clone)]
pub struct Entry {
    pub name: Box<[u16]>,
    pub directory: bool,
    pub(crate) size: u64,
    pub(crate) modified: FILETIME,
    pub(crate) attributes: u32,
}

impl Entry {
    pub fn filename(&self) -> OsString {
        OsString::from_wide(&self.name[..self.name.len() - 1])
    }

    pub(crate) fn write_time(&self) -> u64 {
        u64::from(self.modified.dwHighDateTime) << 32 | u64::from(self.modified.dwLowDateTime)
    }

    pub fn detail(&self, column: i32) -> String {
        match column {
            1 if !self.directory => {
                if self.size < 1024 {
                    format!("{} B", self.size)
                } else {
                    format!("{} KB", grouped(self.size.div_ceil(1024)))
                }
            }
            2 => {
                let mut utc = SYSTEMTIME::default();
                let mut time = SYSTEMTIME::default();
                if unsafe {
                    FileTimeToSystemTime(&self.modified, &mut utc)
                        .and_then(|()| SystemTimeToTzSpecificLocalTimeEx(None, &utc, &mut time))
                }
                .is_ok()
                {
                    format!(
                        "{:04}-{:02}-{:02}  {:02}:{:02}",
                        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute
                    )
                } else {
                    String::new()
                }
            }
            _ => String::new(),
        }
    }
}

pub(crate) fn filename_issue(units: &[u16]) -> Option<&'static str> {
    if units.is_empty() {
        return Some("Enter a name.");
    }
    if units.len() > 255 {
        return Some("Use 255 characters or fewer.");
    }
    if units.iter().any(|unit| {
        *unit < 32
            || b"<>:\"/\\|?*"
                .iter()
                .any(|&symbol| u16::from(symbol) == *unit)
    }) {
        return Some("The name contains a character that Windows does not allow.");
    }
    if units
        .last()
        .is_some_and(|unit| *unit == b' ' as u16 || *unit == b'.' as u16)
    {
        return Some("A name cannot end with a space or a period.");
    }
    let device = String::from_utf16_lossy(units);
    let device = device
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    if matches!(
        device.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].into_iter().any(|prefix| {
        device.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    }) {
        return Some("Windows reserves this name for a device.");
    }
    None
}

pub fn grouped(number: u64) -> String {
    let digits = number.to_string();
    let mut out = String::new();
    for (index, digit) in digits.bytes().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(char::from(digit));
    }
    out
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Column {
    #[default]
    Name,
    Size,
    Modified,
}

impl Column {
    pub fn from_index(index: usize) -> Option<Self> {
        [Self::Name, Self::Size, Self::Modified].get(index).copied()
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Sort {
    pub column: Column,
    pub descending: bool,
}

#[derive(Default)]
pub struct Rows {
    entries: Arc<Vec<Entry>>,
    order: Option<Vec<usize>>,
}

impl Rows {
    pub fn all(entries: Arc<Vec<Entry>>) -> Self {
        Self {
            entries,
            order: None,
        }
    }

    pub fn len(&self) -> usize {
        self.order.as_ref().map_or(self.entries.len(), Vec::len)
    }

    pub fn get(&self, index: usize) -> Option<&Entry> {
        match &self.order {
            Some(order) => order.get(index).map(|&entry| &self.entries[entry]),
            None => self.entries.get(index),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        (0..self.len()).map(|index| self.get(index).unwrap())
    }
}

fn project(
    entries: Arc<Vec<Entry>>,
    sort: Sort,
    query: &[u16],
    current: impl Fn() -> bool,
) -> Option<Rows> {
    if !current() {
        return None;
    }
    if sort == Sort::default() && query.is_empty() {
        return Some(Rows::all(entries));
    }
    let mut order = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if index.is_multiple_of(1024) && !current() {
            return None;
        }
        if query.is_empty()
            || unsafe {
                FindStringOrdinal(
                    FIND_FROMSTART,
                    &entry.name[..entry.name.len() - 1],
                    query,
                    true,
                ) >= 0
            }
        {
            order.push(index);
        }
    }
    if sort != Sort::default() {
        order.sort_unstable_by(|&a, &b| {
            let (left, right) = (&entries[a], &entries[b]);
            right.directory.cmp(&left.directory).then_with(|| {
                let key = match sort.column {
                    Column::Name => a.cmp(&b),
                    Column::Size if !left.directory => left.size.cmp(&right.size),
                    Column::Size => std::cmp::Ordering::Equal,
                    Column::Modified => {
                        let timestamp = |entry: &Entry| {
                            (entry.modified.dwHighDateTime, entry.modified.dwLowDateTime)
                        };
                        timestamp(left).cmp(&timestamp(right))
                    }
                };
                let key = if sort.descending { key.reverse() } else { key };
                key.then_with(|| a.cmp(&b))
            })
        });
    }
    current().then_some(Rows {
        entries,
        order: Some(order),
    })
}

struct Scan {
    id: usize,
    directory: PathBuf,
    preview: bool,
}

struct Projection {
    id: usize,
    entries: Arc<Vec<Entry>>,
    sort: Sort,
    query: Vec<u16>,
}

enum Job {
    Scan(Scan),
    Projection(Projection),
}

#[derive(Default)]
struct Pending {
    scan: Option<Scan>,
    view: Option<Projection>,
    stop: bool,
}

#[derive(Default)]
struct Queue {
    scan_id: AtomicUsize,
    view_id: AtomicUsize,
    pending: Mutex<Pending>,
    ready: Condvar,
}

pub struct Scanner(Arc<Queue>);

pub struct InitialScan {
    queue: Arc<Queue>,
    id: usize,
    directory: PathBuf,
    claimed: AtomicBool,
}

impl InitialScan {
    pub fn start(&self) -> bool {
        if self.claimed.swap(true, Ordering::Relaxed) {
            return false;
        }
        let mut pending = self.queue.pending.lock().unwrap();
        if pending.stop || self.queue.scan_id.load(Ordering::Relaxed) != self.id {
            return false;
        }
        pending.scan = Some(Scan {
            id: self.id,
            directory: self.directory.clone(),
            preview: true,
        });
        self.queue.ready.notify_one();
        true
    }
}

pub enum Update {
    Directory {
        pane: usize,
        id: usize,
        preview: bool,
        entries: Result<Vec<Entry>, windows::core::Error>,
    },
    View {
        pane: usize,
        id: usize,
        rows: Rows,
    },
}

impl Scanner {
    pub fn start(
        pane: usize,
        deliver: impl Fn(Update) + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let queue = Arc::new(Queue::default());
        let deliver = Arc::new(deliver);
        // A spare worker lets a new path open while a previous network call is blocked.
        for index in 0..2 {
            let (queue, deliver) = (queue.clone(), deliver.clone());
            thread::Builder::new()
                .name(format!("directory-{pane}-{index}"))
                .spawn(move || {
                    loop {
                        let job = {
                            let pending = queue.pending.lock().unwrap();
                            let mut pending = queue
                                .ready
                                .wait_while(pending, |pending| {
                                    !pending.stop
                                        && pending.scan.is_none()
                                        && pending.view.is_none()
                                })
                                .unwrap();
                            if pending.stop {
                                break;
                            }
                            pending
                                .view
                                .take()
                                .map(Job::Projection)
                                .or_else(|| pending.scan.take().map(Job::Scan))
                                .unwrap()
                        };
                        match job {
                            Job::Scan(request) => {
                                let current =
                                    || queue.scan_id.load(Ordering::Relaxed) == request.id;
                                let publish = |preview, entries| {
                                    if current() {
                                        deliver(Update::Directory {
                                            pane,
                                            id: request.id,
                                            preview,
                                            entries,
                                        });
                                    }
                                };
                                let entries = scan(&request.directory, current, |rows| {
                                    if request.preview {
                                        let mut first = rows.to_vec();
                                        sort(&mut first);
                                        publish(true, Ok(first));
                                    }
                                });
                                publish(false, entries);
                            }
                            Job::Projection(request) => {
                                let current =
                                    || queue.view_id.load(Ordering::Relaxed) == request.id;
                                if let Some(rows) =
                                    project(request.entries, request.sort, &request.query, current)
                                {
                                    deliver(Update::View {
                                        pane,
                                        id: request.id,
                                        rows,
                                    });
                                }
                            }
                        }
                    }
                })?;
        }
        Ok(Self(queue))
    }

    pub fn prepare_initial(&self, id: usize, directory: PathBuf) -> Arc<InitialScan> {
        let mut pending = self.0.pending.lock().unwrap();
        self.0.scan_id.store(id, Ordering::Relaxed);
        pending.scan = None;
        Arc::new(InitialScan {
            queue: self.0.clone(),
            id,
            directory,
            claimed: AtomicBool::new(false),
        })
    }

    pub fn refresh(&self, id: usize, directory: PathBuf) {
        let mut pending = self.0.pending.lock().unwrap();
        self.0.scan_id.store(id, Ordering::Relaxed);
        pending.scan = Some(Scan {
            id,
            directory,
            preview: false,
        });
        self.0.ready.notify_one();
    }

    pub fn project(&self, id: usize, entries: Arc<Vec<Entry>>, sort: Sort, query: Vec<u16>) {
        let mut pending = self.0.pending.lock().unwrap();
        self.0.view_id.store(id, Ordering::Relaxed);
        pending.view = Some(Projection {
            id,
            entries,
            sort,
            query,
        });
        self.0.ready.notify_one();
    }

    pub fn cancel_view(&self, id: usize) {
        let mut pending = self.0.pending.lock().unwrap();
        self.0.view_id.store(id, Ordering::Relaxed);
        pending.view = None;
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        self.0.pending.lock().unwrap().stop = true;
        self.0.scan_id.fetch_add(1, Ordering::Relaxed);
        self.0.view_id.fetch_add(1, Ordering::Relaxed);
        self.0.ready.notify_all();
    }
}
fn sort(entries: &mut [Entry]) {
    entries.sort_unstable_by(|left, right| {
        right.directory.cmp(&left.directory).then_with(|| {
            let order =
                unsafe { StrCmpLogicalW(PCWSTR(left.name.as_ptr()), PCWSTR(right.name.as_ptr())) };
            order.cmp(&0).then_with(|| left.name.cmp(&right.name))
        })
    });
}

struct Search(HANDLE);

pub(crate) struct DirectoryEntries {
    search: Option<Search>,
    data: WIN32_FIND_DATAW,
    first: bool,
}

impl DirectoryEntries {
    pub fn open(directory: &Path) -> windows::core::Result<Self> {
        let pattern = search_pattern(directory);
        let mut data = WIN32_FIND_DATAW::default();
        let search = match unsafe {
            FindFirstFileExW(
                PCWSTR(pattern.as_ptr()),
                FindExInfoBasic,
                (&mut data as *mut WIN32_FIND_DATAW).cast(),
                FindExSearchNameMatch,
                None,
                FIND_FIRST_EX_LARGE_FETCH,
            )
        } {
            Ok(handle) => Some(Search(handle)),
            Err(error) if error.code() == ERROR_FILE_NOT_FOUND.to_hresult() => None,
            Err(error) => return Err(error),
        };
        Ok(Self {
            search,
            data,
            first: true,
        })
    }

    pub fn next(&mut self) -> windows::core::Result<Option<(&[u16], &WIN32_FIND_DATAW)>> {
        while let Some(search) = &self.search {
            if !std::mem::take(&mut self.first)
                && let Err(error) = unsafe { FindNextFileW(search.0, &mut self.data) }
            {
                self.search = None;
                if error.code() != ERROR_NO_MORE_FILES.to_hresult() {
                    return Err(error);
                }
                break;
            }
            let length = self
                .data
                .cFileName
                .iter()
                .position(|&unit| unit == 0)
                .unwrap();
            let name = &self.data.cFileName[..length];
            if name != [b'.' as u16] && name != [b'.' as u16, b'.' as u16] {
                return Ok(Some((&self.data.cFileName[..=length], &self.data)));
            }
        }
        Ok(None)
    }
}

impl Drop for Search {
    fn drop(&mut self) {
        unsafe {
            let _ = FindClose(self.0);
        }
    }
}

fn search_pattern(directory: &Path) -> Vec<u16> {
    extended_path(&directory.join("*"))
}

pub fn extended_path(path: &Path) -> Vec<u16> {
    let path = wide(path);
    let slash = b'\\' as u16;
    if path.starts_with(&[slash, slash, b'?' as u16, slash])
        || path.starts_with(&[slash, slash, b'.' as u16, slash])
    {
        return path;
    }
    if path.starts_with(&[slash, slash]) {
        let mut extended = wide(r"\\?\UNC\");
        extended.pop();
        extended.extend_from_slice(&path[2..]);
        extended
    } else {
        let mut extended = wide(r"\\?\");
        extended.pop();
        extended.extend(path);
        extended
    }
}

fn scan(
    directory: &Path,
    current: impl Fn() -> bool,
    first_rows: impl FnMut(&[Entry]),
) -> windows::core::Result<Vec<Entry>> {
    let mut entries = read(directory, &current, first_rows)?;
    if current() {
        sort(&mut entries);
    }
    Ok(entries)
}

pub(crate) fn read(
    directory: &Path,
    current: impl Fn() -> bool,
    mut first_rows: impl FnMut(&[Entry]),
) -> windows::core::Result<Vec<Entry>> {
    let mut directory = DirectoryEntries::open(directory)?;
    let mut entries = Vec::new();
    while current()
        && let Some((name, data)) = directory.next()?
    {
        entries.push(Entry {
            name: Box::from(name),
            directory: data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0,
            size: (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow),
            modified: data.ftLastWriteTime,
            attributes: data.dwFileAttributes,
        });
        if entries.len() == 128 {
            first_rows(&entries);
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_windows_path_prefixes_and_extends_local_and_unc_paths() {
        for (input, expected) in [
            (r"C:\", r"\\?\C:\*"),
            (r"C:\a\b", r"\\?\C:\a\b\*"),
            (r"\\server\share\a", r"\\?\UNC\server\share\a\*"),
            (r"\\?\C:\a", r"\\?\C:\a\*"),
            (r"\\?\UNC\server\share\", r"\\?\UNC\server\share\*"),
        ] {
            assert_eq!(search_pattern(Path::new(input)), wide(expected));
        }
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(1000), "1,000");
        assert_eq!(grouped(123456789), "123,456,789");
    }

    #[test]
    fn projects_case_insensitive_matches_and_keeps_folders_first() {
        let entry = |name, directory, size, timestamp| Entry {
            name: wide(name).into_boxed_slice(),
            directory,
            size,
            attributes: 0,
            modified: FILETIME {
                dwLowDateTime: timestamp,
                dwHighDateTime: 0,
            },
        };
        let entries = Arc::new(vec![
            entry("Tape Folder", true, 0, 0),
            entry("Book", false, 2, 0),
            entry("TAPE 1.txt", false, 5, 15),
            entry("Tæpe 2.txt", false, 90, 30),
            entry("tape 3.txt", false, 3, 10),
        ]);
        let query = [b't' as u16, b'a' as u16, b'p' as u16, b'e' as u16];
        let names = |rows: Rows| rows.iter().map(Entry::filename).collect::<Vec<_>>();
        let sort = |column, descending| Sort { column, descending };
        assert_eq!(
            names(project(entries.clone(), sort(Column::Size, false), &query, || true).unwrap()),
            ["Tape Folder", "tape 3.txt", "TAPE 1.txt"],
        );
        assert_eq!(
            names(
                project(
                    entries.clone(),
                    sort(Column::Modified, true),
                    &query,
                    || true
                )
                .unwrap()
            ),
            ["Tape Folder", "TAPE 1.txt", "tape 3.txt"],
        );
        assert!(project(entries.clone(), Sort::default(), &query, || false).is_none());
        let all = project(entries.clone(), Sort::default(), &[], || true).unwrap();
        assert!(Arc::ptr_eq(&entries, &all.entries));
        assert!(all.order.is_none());
    }

    #[test]
    fn initial_scan_starts_once_and_ignores_superseded_paths() {
        let scanner = Scanner(Arc::new(Queue::default()));
        let first = scanner.prepare_initial(1, PathBuf::from(r"C:\First"));
        let second = scanner.prepare_initial(2, PathBuf::from(r"C:\Second"));
        assert!(!first.start());
        assert!(second.start());
        assert!(!second.start());
        let pending = scanner.0.pending.lock().unwrap();
        assert_eq!(
            pending.scan.as_ref().unwrap().directory,
            Path::new(r"C:\Second")
        );
    }
}
