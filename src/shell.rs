use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

use windows::{
    Win32::{
        Foundation::HWND,
        Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoTaskMemFree, CoUninitialize,
        },
        UI::Shell::{
            FOF_WANTNUKEWARNING, FOFX_ADDUNDORECORD, FOFX_RECYCLEONDELETE, FileOperation,
            IFileOperation, IShellItem, SEE_MASK_INVOKEIDLIST, SHCreateItemFromParsingName,
            SHELLEXECUTEINFOW, SHFILEINFOW, SHGFI_SMALLICON, SHGFI_SYSICONINDEX,
            SHGFI_USEFILEATTRIBUTES, SHGSI_SYSICONINDEX, SHGetFileInfoW, SHGetStockIconInfo,
            SHOpenFolderAndSelectItems, SHParseDisplayName, SHSTOCKICONINFO, SIID_DOCNOASSOC,
            SIID_FOLDER, ShellExecuteExW, ShellExecuteW,
        },
        UI::WindowsAndMessaging::SW_SHOWNORMAL,
    },
    core::{PCWSTR, Result, w},
};

use crate::directory::wide;

#[derive(Clone, Copy)]
pub enum Transfer {
    Copy,
    Move,
    Extract,
}

impl Transfer {
    pub fn verb(self) -> &'static str {
        match self {
            Self::Copy => "Copy",
            Self::Move => "Move",
            Self::Extract => "Extract",
        }
    }
}

pub struct Apartment;

impl Apartment {
    pub fn enter() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        }
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

pub(crate) fn item(path: &Path) -> Result<IShellItem> {
    let text = wide(path);
    unsafe { SHCreateItemFromParsingName(PCWSTR(text.as_ptr()), None) }
}

pub enum Operation {
    Rename { source: PathBuf, name: OsString },
    CreateFolder(PathBuf),
    Recycle(Vec<PathBuf>),
}

impl Operation {
    pub fn verb(&self) -> &'static str {
        match self {
            Self::Rename { .. } => "Rename",
            Self::CreateFolder(_) => "New folder",
            Self::Recycle(_) => "Recycle",
        }
    }

    pub fn perform(&self, owner: usize) -> Result<bool> {
        if let Self::CreateFolder(path) = self {
            std::fs::create_dir(path).map_err(|error| {
                windows::core::Error::new(windows::Win32::Foundation::E_FAIL, error.to_string())
            })?;
            return Ok(false);
        }

        let _apartment = Apartment::enter()?;
        unsafe {
            let operation: IFileOperation =
                CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
            operation.SetOwnerWindow(HWND(owner as _))?;
            match self {
                Self::Rename { source, name } => {
                    operation.SetOperationFlags(FOFX_ADDUNDORECORD)?;
                    let name = wide(name);
                    operation.RenameItem(&item(source)?, PCWSTR(name.as_ptr()), None)?;
                }
                Self::Recycle(sources) => {
                    operation.SetOperationFlags(
                        FOFX_RECYCLEONDELETE | FOFX_ADDUNDORECORD | FOF_WANTNUKEWARNING,
                    )?;
                    for source in sources {
                        operation.DeleteItem(&item(source)?, None)?;
                    }
                }
                Self::CreateFolder(_) => unreachable!(),
            }
            let completed = operation.PerformOperations();
            let aborted = operation.GetAnyOperationsAborted();
            completed?;
            Ok(aborted?.as_bool())
        }
    }
}
pub fn open(owner: HWND, path: &Path) -> Result<()> {
    let path = wide(path);
    let result = unsafe {
        ShellExecuteW(
            Some(owner),
            w!("open"),
            PCWSTR(path.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    shell_execute_result(result.0 as isize, "Windows could not open this file.")
}

pub fn properties(owner: HWND, path: &Path) -> Result<()> {
    let path = wide(path);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST,
        hwnd: owner,
        lpVerb: w!("properties"),
        lpFile: PCWSTR(path.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }
}

pub fn open_in_explorer(owner: HWND, path: &Path, select: bool) -> Result<()> {
    if !select {
        let target = wide(path);
        let result = unsafe {
            ShellExecuteW(
                Some(owner),
                w!("explore"),
                PCWSTR(target.as_ptr()),
                None,
                None,
                SW_SHOWNORMAL,
            )
        };
        return shell_execute_result(
            result.0 as isize,
            "Windows could not open this folder in File Explorer.",
        );
    }
    let wide_path = wide(path);
    let mut pidl = std::ptr::null_mut();
    unsafe {
        SHParseDisplayName(PCWSTR(wide_path.as_ptr()), None, &mut pidl, 0, None)?;
        let result = SHOpenFolderAndSelectItems(pidl, None, 0);
        CoTaskMemFree(Some(pidl.cast()));
        result
    }
}

pub fn open_terminal(owner: HWND, directory: &Path) -> Result<()> {
    let dir = wide(directory);
    let candidates: [(PCWSTR, Option<PCWSTR>); 4] = [
        (w!("wt.exe"), Some(w!("-d ."))),
        (w!("pwsh.exe"), None),
        (w!("powershell.exe"), None),
        (w!("cmd.exe"), None),
    ];
    let mut last_code = 0;
    for (program, parameters) in candidates {
        let result = unsafe {
            ShellExecuteW(
                Some(owner),
                w!("open"),
                program,
                parameters.unwrap_or(PCWSTR::null()),
                PCWSTR(dir.as_ptr()),
                SW_SHOWNORMAL,
            )
        };
        let code = result.0 as isize;
        if code > 32 {
            return Ok(());
        }
        last_code = code;
        if code != 2 && code != 3 {
            break;
        }
    }
    shell_execute_result(
        last_code,
        "Windows could not open a terminal in this folder.",
    )
}

fn shell_execute_result(code: isize, fallback: &str) -> Result<()> {
    if code > 32 {
        Ok(())
    } else {
        let message = match code {
            2 => "The file was not found.",
            3 => "The path was not found.",
            5 => "Access was denied.",
            31 => "No app is associated with this file type.",
            _ => fallback,
        };
        Err(windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            message,
        ))
    }
}

const MAX_CACHED_EXTENSIONS: usize = 1024;

pub struct Icons {
    pub list: usize,
    pub folder: i32,
    pub file: i32,
    extensions: RefCell<HashMap<Box<[u16]>, i32>>,
}

impl Icons {
    pub fn load() -> Option<Self> {
        let mut file = SHFILEINFOW::default();
        let mut folder = SHSTOCKICONINFO {
            cbSize: size_of::<SHSTOCKICONINFO>() as u32,
            ..Default::default()
        };
        let list = unsafe {
            SHGetFileInfoW(
                w!("file"),
                FILE_ATTRIBUTE_NORMAL,
                Some(&mut file),
                size_of::<SHFILEINFOW>() as u32,
                SHGFI_SYSICONINDEX | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES,
            )
        };
        if list == 0 {
            return None;
        }
        let mut document = folder;
        unsafe {
            SHGetStockIconInfo(SIID_FOLDER, SHGSI_SYSICONINDEX, &mut folder).ok()?;
            SHGetStockIconInfo(SIID_DOCNOASSOC, SHGSI_SYSICONINDEX, &mut document).ok()?;
        }
        Some(Self {
            list,
            folder: folder.iSysImageIndex,
            file: document.iSysImageIndex,
            extensions: RefCell::new(HashMap::new()),
        })
    }

    pub fn for_entry(&self, name: &[u16], directory: bool) -> i32 {
        if directory {
            return self.folder;
        }
        let Some(extension) = extension_key(name) else {
            return self.file;
        };
        if let Some(&icon) = self.extensions.borrow().get(extension.as_slice()) {
            return icon;
        }
        let mut probe = Vec::with_capacity(extension.len() + 3);
        probe.push(u16::from(b'f'));
        probe.push(u16::from(b'.'));
        probe.extend_from_slice(&extension);
        probe.push(0);
        let mut info = SHFILEINFOW::default();
        let icon = if unsafe {
            SHGetFileInfoW(
                PCWSTR(probe.as_ptr()),
                FILE_ATTRIBUTE_NORMAL,
                Some(&mut info),
                size_of::<SHFILEINFOW>() as u32,
                SHGFI_SYSICONINDEX | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES,
            )
        } != 0
        {
            info.iIcon
        } else {
            self.file
        };
        let mut cache = self.extensions.borrow_mut();
        if cache.len() < MAX_CACHED_EXTENSIONS {
            cache.insert(extension.into_boxed_slice(), icon);
        }
        icon
    }
}

fn extension_key(name: &[u16]) -> Option<Vec<u16>> {
    let bare = name.strip_suffix(&[0]).unwrap_or(name);
    let dot = bare.iter().rposition(|&unit| unit == u16::from(b'.'))?;
    if dot == 0 || dot + 1 >= bare.len() {
        return None;
    }
    let ext = &bare[dot + 1..];
    if ext.len() > 32 {
        return None;
    }
    Some(
        ext.iter()
            .map(|&unit| {
                if (u16::from(b'A')..=u16::from(b'Z')).contains(&unit) {
                    unit + 32
                } else {
                    unit
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_normalized_lowercase_extension_keys() {
        let key =
            |text: &str| extension_key(&wide(text)).map(|units| String::from_utf16_lossy(&units));
        assert_eq!(key("photo.JPG").as_deref(), Some("jpg"));
        assert_eq!(key("archive.tar.GZ").as_deref(), Some("gz"));
        assert_eq!(key(".gitignore"), None);
        assert_eq!(key("Makefile"), None);
        assert_eq!(key("trailing."), None);
    }
}
