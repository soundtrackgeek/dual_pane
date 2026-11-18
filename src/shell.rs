use std::path::{Path, PathBuf};

use windows::{
    Win32::{
        Foundation::HWND,
        Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
        UI::Shell::{
            FOF_NOCONFIRMMKDIR, FileOperation, IFileOperation, IShellItem,
            SHCreateItemFromParsingName, SHFILEINFOW, SHGFI_SMALLICON, SHGFI_SYSICONINDEX,
            SHGFI_USEFILEATTRIBUTES, SHGSI_SYSICONINDEX, SHGetFileInfoW, SHGetStockIconInfo,
            SHSTOCKICONINFO, SIID_DOCNOASSOC, SIID_FOLDER, ShellExecuteW,
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
}

impl Transfer {
    pub fn verb(self) -> &'static str {
        match self {
            Self::Copy => "Copy",
            Self::Move => "Move",
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

fn item(path: &Path) -> Result<IShellItem> {
    let text = wide(path);
    unsafe { SHCreateItemFromParsingName(PCWSTR(text.as_ptr()), None) }
}

pub fn transfer(
    owner: isize,
    action: Transfer,
    sources: &[PathBuf],
    destination: &Path,
) -> Result<bool> {
    let _apartment = Apartment::enter()?;
    let destination = item(destination)?;
    unsafe {
        let operation: IFileOperation =
            CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
        operation.SetOwnerWindow(HWND(owner as _))?;
        operation.SetOperationFlags(FOF_NOCONFIRMMKDIR)?;
        for source in sources {
            let source = item(source)?;
            match action {
                Transfer::Copy => {
                    operation.CopyItem(&source, &destination, PCWSTR::null(), None)?
                }
                Transfer::Move => {
                    operation.MoveItem(&source, &destination, PCWSTR::null(), None)?
                }
            }
        }
        operation.PerformOperations()?;
        Ok(operation.GetAnyOperationsAborted()?.as_bool())
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
    let code = result.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        let message = match code {
            2 => "The file was not found.",
            3 => "The path was not found.",
            5 => "Access was denied.",
            31 => "No app is associated with this file type.",
            _ => "Windows could not open this file.",
        };
        Err(windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            message,
        ))
    }
}

pub struct Icons {
    pub list: usize,
    pub folder: i32,
    pub file: i32,
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
        })
    }
}
