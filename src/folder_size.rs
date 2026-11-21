use std::{
    collections::VecDeque,
    ffi::OsString,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use windows::{
    Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, GetFileAttributesW,
        INVALID_FILE_ATTRIBUTES,
    },
    core::PCWSTR,
};

use crate::directory::{DirectoryEntries, extended_path};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FolderSize {
    pub bytes: u64,
    pub partial: bool,
}

pub struct SizeResult {
    pub pane: usize,
    pub location: usize,
    pub name: Box<[u16]>,
    pub size: FolderSize,
}

type Deliver = Arc<dyn Fn(SizeResult) + Send + Sync>;

#[derive(Default)]
struct Pending {
    items: VecDeque<(usize, Box<[u16]>, PathBuf)>,
    started: bool,
    stop: bool,
}

#[derive(Default)]
struct Queue {
    location: AtomicUsize,
    pending: Mutex<Pending>,
    ready: Condvar,
}

pub struct Sizer {
    pane: usize,
    queue: Arc<Queue>,
    deliver: Deliver,
}

impl Sizer {
    pub fn new(pane: usize, deliver: impl Fn(SizeResult) + Send + Sync + 'static) -> Self {
        Self {
            pane,
            queue: Arc::new(Queue::default()),
            deliver: Arc::new(deliver),
        }
    }

    pub fn cancel(&self, location: usize) {
        let mut pending = self.queue.pending.lock().unwrap();
        self.queue.location.store(location, Ordering::Relaxed);
        pending.items.clear();
    }

    pub fn enqueue(&self, location: usize, items: Vec<(Box<[u16]>, PathBuf)>) {
        if items.is_empty() {
            return;
        }
        let mut pending = self.queue.pending.lock().unwrap();
        if pending.stop {
            return;
        }
        let previous = self.queue.location.swap(location, Ordering::Relaxed);
        if previous != location {
            pending.items.clear();
        }
        for (name, path) in items {
            if !pending
                .items
                .iter()
                .any(|(loc, existing, _)| *loc == location && *existing == name)
            {
                pending.items.push_back((location, name, path));
            }
        }
        if !pending.started {
            pending.started = true;
            let pane = self.pane;
            let queue = self.queue.clone();
            let deliver = self.deliver.clone();
            if thread::Builder::new()
                .name(format!("folder-size-{pane}"))
                .spawn(move || worker(pane, queue, deliver))
                .is_err()
            {
                pending.started = false;
                pending.items.clear();
                return;
            }
        }
        self.queue.ready.notify_one();
    }
}

impl Drop for Sizer {
    fn drop(&mut self) {
        let mut pending = self.queue.pending.lock().unwrap();
        pending.stop = true;
        pending.items.clear();
        self.queue.location.fetch_add(1, Ordering::Relaxed);
        self.queue.ready.notify_all();
    }
}

fn worker(pane: usize, queue: Arc<Queue>, deliver: Deliver) {
    loop {
        let (location, name, path) = {
            let pending = queue.pending.lock().unwrap();
            let mut pending = queue
                .ready
                .wait_while(pending, |pending| !pending.stop && pending.items.is_empty())
                .unwrap();
            if pending.stop {
                break;
            }
            pending.items.pop_front().unwrap()
        };
        let current = || queue.location.load(Ordering::Relaxed) == location;
        if !current() {
            continue;
        }
        if let Some(size) = measure_folder(&path, current)
            && current()
        {
            deliver(SizeResult {
                pane,
                location,
                name,
                size,
            });
        }
    }
}

pub fn measure_folder(path: &Path, current: impl Fn() -> bool) -> Option<FolderSize> {
    if !current() {
        return None;
    }
    let root_wide = extended_path(path);
    let attrs = unsafe { GetFileAttributesW(PCWSTR(root_wide.as_ptr())) };
    if attrs == INVALID_FILE_ATTRIBUTES || (attrs & FILE_ATTRIBUTE_REPARSE_POINT.0) != 0 {
        return Some(FolderSize {
            bytes: 0,
            partial: true,
        });
    }

    let mut bytes = 0_u64;
    let mut partial = false;
    let mut stack = vec![path.to_path_buf()];
    let mut seen = 0_usize;

    while let Some(directory) = stack.pop() {
        if !current() {
            return None;
        }
        let mut entries = match DirectoryEntries::open(&directory) {
            Ok(entries) => entries,
            Err(_) => {
                partial = true;
                continue;
            }
        };
        loop {
            seen += 1;
            if seen.is_multiple_of(256) && !current() {
                return None;
            }
            match entries.next() {
                Ok(Some((name, data))) => {
                    let is_dir = data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
                    if is_dir {
                        if data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
                            partial = true;
                        } else {
                            let child = OsString::from_wide(&name[..name.len() - 1]);
                            stack.push(directory.join(child));
                        }
                    } else {
                        let size =
                            u64::from(data.nFileSizeHigh) << 32 | u64::from(data.nFileSizeLow);
                        bytes = bytes.saturating_add(size);
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    partial = true;
                    break;
                }
            }
        }
    }

    current().then_some(FolderSize { bytes, partial })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn measures_nested_folder_tree() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("sub").join("deep");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir(root.path().join("empty")).unwrap();
        fs::write(root.path().join("a.bin"), vec![1_u8; 1_500]).unwrap();
        fs::write(nested.join("b.bin"), vec![2_u8; 2_750]).unwrap();

        let size = measure_folder(root.path(), || true).unwrap();
        assert_eq!(
            size,
            FolderSize {
                bytes: 4_250,
                partial: false,
            }
        );
    }

    #[test]
    fn cancellation_returns_none() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("a.bin"), b"hello").unwrap();
        assert!(measure_folder(root.path(), || false).is_none());
    }
}
