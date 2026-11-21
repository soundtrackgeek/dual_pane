use std::{
    ffi::OsString,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Component, Path, PathBuf, Prefix},
};

use windows::{
    Win32::{
        Foundation::*,
        Globalization::{CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar},
        System::{Com::*, DataExchange::*, Memory::*, Ole::*},
        UI::Shell::SHCreateStdEnumFmtEtc,
    },
    core::{BOOL, Error, HRESULT, Ref, Result, implement, w},
};

const HEADER: usize = 20;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_FILES: usize = 100_000;

pub struct Ole;

impl Ole {
    pub fn enter() -> Result<Self> {
        unsafe {
            OleInitialize(None)?;
        }
        Ok(Self)
    }
}

impl Drop for Ole {
    fn drop(&mut self) {
        unsafe {
            OleUninitialize();
        }
    }
}

pub fn preferred_effect() -> u32 {
    unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) }
}

fn performed_effect() -> u32 {
    unsafe { RegisterClipboardFormatW(w!("Performed DropEffect")) }
}

fn invalid() -> Error {
    Error::new(
        E_INVALIDARG,
        "The file list on the clipboard or drop is invalid.",
    )
}

fn valid_path(path: &Path) -> bool {
    if !path.is_absolute() || path.file_name().is_none() {
        return false;
    }
    path.components().all(|component| match component {
        Component::Prefix(prefix) => matches!(
            prefix.kind(),
            Prefix::Disk(_) | Prefix::UNC(..) | Prefix::VerbatimDisk(_) | Prefix::VerbatimUNC(..)
        ),
        Component::RootDir => true,
        Component::Normal(name) => {
            crate::directory::filename_issue(&name.encode_wide().collect::<Vec<_>>()).is_none()
        }
        _ => false,
    })
}

fn encode(paths: &[PathBuf]) -> Result<Vec<u8>> {
    if paths.is_empty() || paths.len() > MAX_FILES {
        return Err(invalid());
    }
    let mut data = vec![0; HEADER];
    data[..4].copy_from_slice(&(HEADER as u32).to_le_bytes());
    data[16] = 1; // DROPFILES.fWide
    for path in paths {
        if !valid_path(path) {
            return Err(invalid());
        }
        for value in path.as_os_str().encode_wide().chain(std::iter::once(0)) {
            data.extend_from_slice(&value.to_le_bytes());
        }
        if data.len() + 2 > MAX_BYTES {
            return Err(invalid());
        }
    }
    data.extend_from_slice(&[0, 0]);
    Ok(data)
}

fn decode(data: &[u8]) -> Result<Vec<PathBuf>> {
    if data.len() < HEADER || data.len() > MAX_BYTES {
        return Err(invalid());
    }
    let offset = u32::from_le_bytes(data[..4].try_into().unwrap()) as usize;
    if offset < HEADER || offset >= data.len() {
        return Err(invalid());
    }
    let wide = data[16..20] != [0; 4];
    let mut paths = Vec::new();
    if wide {
        let mut name = Vec::new();
        for pair in data[offset..].chunks_exact(2) {
            let unit = u16::from_le_bytes([pair[0], pair[1]]);
            if unit != 0 {
                if name.len() == 32767 {
                    return Err(invalid());
                }
                name.push(unit);
            } else if name.is_empty() {
                return (!paths.is_empty()).then_some(paths).ok_or_else(invalid);
            } else {
                push_path(&mut paths, PathBuf::from(OsString::from_wide(&name)))?;
                name.clear();
            }
        }
    } else {
        let mut start = offset;
        for end in offset..data.len() {
            if data[end] != 0 {
                continue;
            }
            if start == end {
                return (!paths.is_empty()).then_some(paths).ok_or_else(invalid);
            }
            let name = &data[start..end];
            if name.len() > 32767 {
                return Err(invalid());
            }
            let flags = MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0);
            let size = unsafe { MultiByteToWideChar(CP_ACP, flags, name, None) };
            if size <= 0 {
                return Err(invalid());
            }
            let mut units = vec![0; size as usize];
            if unsafe { MultiByteToWideChar(CP_ACP, flags, name, Some(&mut units)) } != size {
                return Err(invalid());
            }
            push_path(&mut paths, PathBuf::from(OsString::from_wide(&units)))?;
            start = end + 1;
        }
    }
    Err(invalid())
}

fn push_path(paths: &mut Vec<PathBuf>, path: PathBuf) -> Result<()> {
    if paths.len() == MAX_FILES || !valid_path(&path) {
        return Err(invalid());
    }
    paths.push(path);
    Ok(())
}

struct Memory(HGLOBAL);

impl Memory {
    fn new(bytes: &[u8]) -> Result<Self> {
        unsafe {
            let memory = Self(GlobalAlloc(GMEM_MOVEABLE, bytes.len())?);
            let target = GlobalLock(memory.0);
            if target.is_null() {
                return Err(Error::from_thread());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), target.cast(), bytes.len());
            let _ = GlobalUnlock(memory.0);
            Ok(memory)
        }
    }

    fn release(self) -> HGLOBAL {
        let handle = self.0;
        std::mem::forget(self);
        handle
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        unsafe {
            let _ = GlobalFree(Some(self.0));
        }
    }
}

fn read_memory<T>(handle: HGLOBAL, read: impl FnOnce(&[u8]) -> Result<T>) -> Result<T> {
    unsafe {
        let size = GlobalSize(handle);
        if size == 0 || size > MAX_BYTES {
            return Err(invalid());
        }
        let pointer = GlobalLock(handle);
        if pointer.is_null() {
            return Err(invalid());
        }
        let result = read(std::slice::from_raw_parts(pointer.cast(), size));
        let _ = GlobalUnlock(handle);
        result
    }
}

struct Clipboard;

impl Clipboard {
    fn open(window: HWND) -> Result<Self> {
        unsafe {
            OpenClipboard(Some(window))?;
        }
        Ok(Self)
    }

    fn put(&self, format: u32, memory: Memory) -> Result<()> {
        unsafe {
            SetClipboardData(format, Some(HANDLE(memory.0.0)))?;
        }
        memory.release();
        Ok(())
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

pub fn clipboard_write(window: HWND, paths: &[PathBuf], cut: bool) -> Result<u32> {
    let format = preferred_effect();
    if format == 0 {
        return Err(Error::from_thread());
    }
    let files = Memory::new(&encode(paths)?)?;
    let effect = if cut {
        DROPEFFECT_MOVE
    } else {
        DROPEFFECT_COPY
    };
    let effect = Memory::new(&effect.0.to_le_bytes())?;
    let clipboard = Clipboard::open(window)?;
    unsafe {
        EmptyClipboard()?;
    }
    clipboard.put(CF_HDROP.0.into(), files)?;
    if let Err(error) = clipboard.put(format, effect) {
        unsafe {
            let _ = EmptyClipboard();
        }
        return Err(error);
    }
    Ok(unsafe { GetClipboardSequenceNumber() })
}

pub fn format_paths_text(paths: &[PathBuf]) -> OsString {
    let mut out = OsString::new();
    for (index, path) in paths.iter().enumerate() {
        if index > 0 {
            out.push("\r\n");
        }
        out.push(path.as_os_str());
    }
    out
}

pub fn clipboard_write_text(window: HWND, text: &std::ffi::OsStr) -> Result<u32> {
    let units: Vec<u16> = text.encode_wide().chain(std::iter::once(0)).collect();
    let byte_len = units.len().saturating_mul(2);
    if byte_len > MAX_BYTES {
        return Err(Error::new(
            E_INVALIDARG,
            "The path text exceeds the clipboard size limit.",
        ));
    }
    let bytes = unsafe { std::slice::from_raw_parts(units.as_ptr().cast::<u8>(), byte_len) };
    let memory = Memory::new(bytes)?;
    let clipboard = Clipboard::open(window)?;
    unsafe {
        EmptyClipboard()?;
    }
    clipboard.put(CF_UNICODETEXT.0.into(), memory)?;
    Ok(unsafe { GetClipboardSequenceNumber() })
}

pub fn clipboard_read(window: HWND) -> Result<(Vec<PathBuf>, bool, u32)> {
    let _clipboard = Clipboard::open(window)?;
    let paths = unsafe { GetClipboardData(CF_HDROP.0.into()) }.map_err(|_| {
        Error::new(
            DV_E_FORMATETC,
            "The Windows clipboard does not contain file paths.",
        )
    })?;
    let paths = read_memory(HGLOBAL(paths.0), decode)?;
    let cut = unsafe { GetClipboardData(preferred_effect()) }
        .ok()
        .and_then(|handle| {
            read_memory(HGLOBAL(handle.0), |bytes| {
                let effect = bytes.get(..4).ok_or_else(invalid)?;
                Ok(u32::from_le_bytes(effect.try_into().unwrap()) == DROPEFFECT_MOVE.0)
            })
            .ok()
        })
        .unwrap_or(false);
    Ok((paths, cut, unsafe { GetClipboardSequenceNumber() }))
}

pub fn clear_if_current(window: HWND, sequence: u32) {
    if let Ok(_clipboard) = Clipboard::open(window)
        && unsafe { GetClipboardSequenceNumber() } == sequence
    {
        unsafe {
            let _ = EmptyClipboard();
        }
    }
}

fn format_for(format: u32) -> FORMATETC {
    FORMATETC {
        cfFormat: format as u16,
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
        ..FORMATETC::default()
    }
}

pub fn has_files(data: &IDataObject) -> bool {
    unsafe { data.QueryGetData(&format_for(CF_HDROP.0.into())).is_ok() }
}

pub fn drop_files(data: &IDataObject) -> Result<Vec<PathBuf>> {
    let mut medium = unsafe { data.GetData(&format_for(CF_HDROP.0.into()))? };
    let result = if medium.tymed == TYMED_HGLOBAL.0 as u32 {
        read_memory(unsafe { medium.u.hGlobal }, decode)
    } else {
        Err(invalid())
    };
    unsafe {
        ReleaseStgMedium(&mut medium);
    }
    result
}

pub fn no_source_deletion(data: &IDataObject) {
    let format = performed_effect();
    if format != 0
        && let Ok(memory) = Memory::new(&DROPEFFECT_NONE.0.to_le_bytes())
    {
        let medium = STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: memory.0 },
            ..STGMEDIUM::default()
        };
        // With fRelease false the data object must copy the value if it needs it.
        unsafe {
            let _ = data.SetData(&format_for(format), &medium, false);
        }
    }
}

pub fn drag_data(paths: &[PathBuf]) -> Result<IDataObject> {
    Ok(DragData(encode(paths)?).into())
}

#[implement(IDataObject)]
struct DragData(Vec<u8>);

impl IDataObject_Impl for DragData_Impl {
    fn GetData(&self, request: *const FORMATETC) -> Result<STGMEDIUM> {
        self.QueryGetData(request).ok()?;
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 {
                hGlobal: Memory::new(&self.0)?.release(),
            },
            ..STGMEDIUM::default()
        })
    }

    fn QueryGetData(&self, request: *const FORMATETC) -> HRESULT {
        if let Some(request) = unsafe { request.as_ref() }
            && request.cfFormat == CF_HDROP.0
            && request.dwAspect == DVASPECT_CONTENT.0
            && request.lindex == -1
            && request.tymed & TYMED_HGLOBAL.0 as u32 != 0
        {
            S_OK
        } else {
            DV_E_FORMATETC
        }
    }

    fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn GetCanonicalFormatEtc(&self, _: *const FORMATETC, output: *mut FORMATETC) -> HRESULT {
        if let Some(output) = unsafe { output.as_mut() } {
            output.ptd = std::ptr::null_mut();
        }
        DATA_S_SAMEFORMATETC
    }

    fn SetData(
        &self,
        request: *const FORMATETC,
        medium: *const STGMEDIUM,
        release: BOOL,
    ) -> Result<()> {
        let request = unsafe { request.as_ref() }.ok_or(DV_E_FORMATETC)?;
        let accepted = [performed_effect(), unsafe {
            RegisterClipboardFormatW(w!("Logical Performed DropEffect"))
        }]
        .into_iter()
        .any(|format| format != 0 && u32::from(request.cfFormat) == format);
        if !accepted || medium.is_null() {
            return Err(DV_E_FORMATETC.into());
        }
        if release.as_bool() {
            unsafe {
                ReleaseStgMedium(medium.cast_mut());
            }
        }
        Ok(())
    }

    fn EnumFormatEtc(&self, direction: u32) -> Result<IEnumFORMATETC> {
        if direction != DATADIR_GET.0 as u32 {
            return Err(E_NOTIMPL.into());
        }
        unsafe { SHCreateStdEnumFmtEtc(&[format_for(CF_HDROP.0.into())]) }
    }

    fn DAdvise(&self, _: *const FORMATETC, _: u32, _: Ref<IAdviseSink>) -> Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
    fn DUnadvise(&self, _: u32) -> Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
    fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_list_round_trips_unicode_and_rejects_unterminated_or_unsafe_paths() {
        let paths = [
            PathBuf::from(r"C:\æ\🦀.txt"),
            PathBuf::from(r"\\server\share\album"),
        ];
        let mut data = encode(&paths).unwrap();
        assert_eq!(decode(&data).unwrap(), paths);
        data.truncate(data.len() - 2);
        assert!(decode(&data).is_err());
        assert!(encode(&[PathBuf::from(r"C:\Folder\..\test.txt")]).is_err());
        assert!(encode(&[PathBuf::from(r"\\.\PhysicalDrive0")]).is_err());
    }
}
