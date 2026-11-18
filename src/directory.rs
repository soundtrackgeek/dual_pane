use std::{
    ffi::{OsStr, OsString},
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use windows::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_FILES, FILETIME, HANDLE, SYSTEMTIME},
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
    size: u64,
    modified: FILETIME,
}

impl Entry {
    pub fn filename(&self) -> OsString {
        OsString::from_wide(&self.name[..self.name.len() - 1])
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

struct Request {
    id: usize,
    directory: PathBuf,
    preview: bool,
}

#[derive(Default)]
struct Queue {
    latest: AtomicUsize,
    pending: Mutex<Option<Request>>,
    ready: Condvar,
}

pub struct Scanner(Arc<Queue>);

pub struct Update {
    pub pane: usize,
    pub id: usize,
    pub preview: bool,
    pub entries: Result<Vec<Entry>, windows::core::Error>,
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
                        let request = {
                            let pending = queue.pending.lock().unwrap();
                            let mut pending = queue
                                .ready
                                .wait_while(pending, |request| request.is_none())
                                .unwrap();
                            pending.take().unwrap()
                        };
                        let current = || queue.latest.load(Ordering::Relaxed) == request.id;
                        let publish = |preview, entries| {
                            if current() {
                                deliver(Update {
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
                })?;
        }
        Ok(Self(queue))
    }

    pub fn request(&self, id: usize, directory: PathBuf, preview: bool) {
        let mut pending = self.0.pending.lock().unwrap();
        self.0.latest.store(id, Ordering::Relaxed);
        *pending = Some(Request {
            id,
            directory,
            preview,
        });
        self.0.ready.notify_one();
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

impl Drop for Search {
    fn drop(&mut self) {
        unsafe {
            let _ = FindClose(self.0);
        }
    }
}

fn search_pattern(directory: &Path) -> Vec<u16> {
    let path = wide(directory.join("*"));
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
    mut first_rows: impl FnMut(&[Entry]),
) -> windows::core::Result<Vec<Entry>> {
    let pattern = search_pattern(directory);
    let mut data = WIN32_FIND_DATAW::default();
    let handle = unsafe {
        FindFirstFileExW(
            PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            (&mut data as *mut WIN32_FIND_DATAW).cast(),
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    let search = match handle {
        Ok(handle) => Search(handle),
        Err(error) if error.code() == ERROR_FILE_NOT_FOUND.to_hresult() => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };

    let mut entries = Vec::new();
    while current() {
        let length = data.cFileName.iter().position(|&value| value == 0).unwrap();
        let name = &data.cFileName[..length];
        if name != [b'.' as u16] && name != [b'.' as u16, b'.' as u16] {
            entries.push(Entry {
                name: Box::from(&data.cFileName[..=length]),
                directory: data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0,
                size: (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow),
                modified: data.ftLastWriteTime,
            });
            if entries.len() == 128 {
                first_rows(&entries);
            }
        }
        if let Err(error) = unsafe { FindNextFileW(search.0, &mut data) } {
            if error.code() != ERROR_NO_MORE_FILES.to_hresult() {
                return Err(error);
            }
            break;
        }
    }
    if current() {
        sort(&mut entries);
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
}
