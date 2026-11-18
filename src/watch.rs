use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
};

use windows::{
    Win32::{
        Foundation::{ERROR_NOTIFY_ENUM_DIR, HANDLE, WAIT_OBJECT_0},
        Storage::FileSystem::{
            CreateFileW, FILE_ACTION_RENAMED_NEW_NAME, FILE_ACTION_RENAMED_OLD_NAME,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
            FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
            FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadDirectoryChangesW,
        },
        System::{
            IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
            Threading::{
                CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForMultipleObjects,
                WaitForSingleObject,
            },
        },
    },
    core::{PCWSTR, Result},
};

use crate::directory::{InitialScan, extended_path};

pub type Rename = (Vec<u16>, Vec<u16>);

pub enum Notice {
    Armed { reconcile: bool },
    Changed(Vec<Rename>),
    Unavailable,
}

enum Target {
    Folder(usize, PathBuf, Option<Arc<InitialScan>>),
    Idle,
    Stop,
}

struct Mailbox {
    signal: OwnedHandle,
    pending: Mutex<Option<Target>>,
}

pub struct Watcher(Arc<Mailbox>);

impl Watcher {
    pub fn start(pane: usize, send: impl Fn(usize, Notice) + Send + 'static) -> Result<Self> {
        let mailbox = Arc::new(Mailbox {
            signal: owned(unsafe { CreateEventW(None, true, false, PCWSTR::null())? }),
            pending: Mutex::default(),
        });
        let worker = mailbox.clone();
        let completed = owned(unsafe { CreateEventW(None, true, false, PCWSTR::null())? });
        thread::Builder::new()
            .name(format!("watch-{pane}"))
            .spawn(move || {
                let request = handle(&worker.signal);
                let completed = handle(&completed);
                loop {
                    let target = {
                        let mut pending = worker.pending.lock().unwrap();
                        unsafe {
                            let _ = ResetEvent(request);
                        }
                        pending.take()
                    };
                    match target {
                        Some(Target::Folder(id, path, initial)) => {
                            if monitor(&path, request, completed, initial.as_deref(), |message| {
                                send(id, message)
                            })
                            .is_err()
                            {
                                if let Some(initial) = initial {
                                    initial.start();
                                }
                                send(id, Notice::Unavailable);
                            }
                        }
                        Some(Target::Stop) => break,
                        Some(Target::Idle) => {}
                        None => {
                            if unsafe { WaitForSingleObject(request, INFINITE) } != WAIT_OBJECT_0 {
                                break;
                            }
                        }
                    }
                }
            })
            .map_err(|error| {
                windows::core::Error::new(windows::Win32::Foundation::E_FAIL, error.to_string())
            })?;
        Ok(Self(mailbox))
    }

    pub fn watch(&self, id: usize, path: PathBuf, initial: Option<Arc<InitialScan>>) {
        self.request(Target::Folder(id, path, initial));
    }

    pub fn suspend(&self) {
        self.request(Target::Idle);
    }

    fn request(&self, target: Target) {
        let mut pending = self.0.pending.lock().unwrap();
        *pending = Some(target);
        unsafe {
            let _ = SetEvent(handle(&self.0.signal));
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.request(Target::Stop);
    }
}

fn owned(handle: HANDLE) -> OwnedHandle {
    unsafe { OwnedHandle::from_raw_handle(handle.0) }
}

fn handle(owner: &OwnedHandle) -> HANDLE {
    HANDLE(owner.as_raw_handle())
}

fn monitor(
    path: &Path,
    request: HANDLE,
    completed: HANDLE,
    initial: Option<&InitialScan>,
    send: impl Fn(Notice),
) -> Result<()> {
    let path = extended_path(path);
    let directory = owned(unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            None,
        )?
    });
    let directory_handle = handle(&directory);
    // The redirector allows up to 64 KiB, and the buffer has to be DWORD aligned.
    let mut records = vec![0_u32; 16 * 1024];
    let mut first_read = true;
    let mut previous_name = None;
    loop {
        let mut operation = OVERLAPPED {
            hEvent: completed,
            ..Default::default()
        };
        unsafe {
            ResetEvent(completed)?;
            ReadDirectoryChangesW(
                directory_handle,
                records.as_mut_ptr().cast(),
                (records.len() * size_of::<u32>()) as u32,
                false,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_DIR_NAME
                    | FILE_NOTIFY_CHANGE_SIZE
                    | FILE_NOTIFY_CHANGE_LAST_WRITE,
                None,
                Some(&mut operation),
                None,
            )?;
        }
        if first_read {
            first_read = false;
            let reconcile = !initial.is_some_and(InitialScan::start);
            send(Notice::Armed { reconcile });
        }
        let mut bytes = 0;
        let wait = unsafe { WaitForMultipleObjects(&[request, completed], false, INFINITE) };
        if wait.0 != WAIT_OBJECT_0.0 + 1 {
            // The kernel may still hold both addresses when cancellation is requested.
            unsafe {
                let _ = CancelIoEx(directory_handle, Some(&operation));
                let _ = GetOverlappedResult(directory_handle, &operation, &mut bytes, true);
            }
            return if wait == WAIT_OBJECT_0 {
                Ok(())
            } else {
                Err(windows::core::Error::from_thread())
            };
        }
        let result =
            unsafe { GetOverlappedResult(directory_handle, &operation, &mut bytes, false) };
        let changes = match result {
            Ok(()) => {
                let data = unsafe {
                    std::slice::from_raw_parts(records.as_ptr().cast::<u8>(), bytes as usize)
                };
                if data.is_empty() {
                    previous_name = None;
                }
                renames(data, &mut previous_name)
            }
            Err(error) if error.code() == ERROR_NOTIFY_ENUM_DIR.to_hresult() => {
                previous_name = None;
                Vec::new()
            }
            Err(error) => return Err(error),
        };
        send(Notice::Changed(changes));
    }
}

fn renames(buffer: &[u8], old: &mut Option<Vec<u16>>) -> Vec<Rename> {
    let mut names = Vec::new();
    let mut record = buffer;
    while record.len() >= 12 {
        let number = |offset| u32::from_le_bytes(record[offset..offset + 4].try_into().unwrap());
        let next = number(0) as usize;
        let action = number(4);
        let length = number(8) as usize;
        let Some(name) = record.get(12..12 + length) else {
            break;
        };
        let units = || {
            name.chunks_exact(2)
                .map(|part| u16::from_le_bytes([part[0], part[1]]))
                .chain(std::iter::once(0))
                .collect()
        };
        if action == FILE_ACTION_RENAMED_OLD_NAME.0 {
            *old = Some(units());
        } else if let Some(previous) = old.take()
            && action == FILE_ACTION_RENAMED_NEW_NAME.0
        {
            names.push((previous, units()));
        }
        if next == 0 || next < 12 + length || next >= record.len() {
            break;
        }
        record = &record[next..];
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::wide;

    #[test]
    fn pairs_rename_notifications_and_ignores_truncated_records() {
        let mut bytes = Vec::new();
        for (next, action, name) in [(16_u32, 4_u32, 'æ'), (0, 5, 'ø')] {
            bytes.extend(next.to_le_bytes());
            bytes.extend(action.to_le_bytes());
            bytes.extend(2_u32.to_le_bytes());
            bytes.extend((name as u16).to_le_bytes());
            bytes.extend([0, 0]);
        }
        assert_eq!(renames(&bytes, &mut None), vec![(wide("æ"), wide("ø"))]);
        assert!(renames(&bytes[..28], &mut None).is_empty());
        assert!(renames(&[], &mut None).is_empty());
        let mut previous = None;
        assert!(renames(&bytes[..16], &mut previous).is_empty());
        assert_eq!(
            renames(&bytes[16..], &mut previous),
            vec![(wide("æ"), wide("ø"))]
        );
    }
}
