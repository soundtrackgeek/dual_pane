use super::*;
use navigation::{Bookmark, MAX_BOOKMARKS};
use windows::Win32::{
    System::Com::CoTaskMemFree,
    UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Profile,
        KF_FLAG_DEFAULT, SHGetKnownFolderPath,
    },
};

const SAVE: u16 = 460;
const REMOVE: u16 = 461;
const LIST: u16 = 462;
const PATH_PREVIEW: u16 = 463;
const OPEN_FIRST: isize = 470;
const REMOVE_FIRST: isize = 550;

struct Entry {
    label: OsString,
    path: PathBuf,
    saved_index: Option<usize>,
}

struct Picker<'a> {
    entries: &'a [Entry],
    font: HFONT,
}

fn known_folders() -> Vec<Entry> {
    let mut entries = Vec::new();
    for (name, id) in [
        ("User Profile", &FOLDERID_Profile),
        ("Desktop", &FOLDERID_Desktop),
        ("Downloads", &FOLDERID_Downloads),
        ("Documents", &FOLDERID_Documents),
    ] {
        if let Some(path) = known_folder_path(id) {
            entries.push(Entry {
                label: OsString::from(name),
                path,
                saved_index: None,
            });
        }
    }
    entries
}

fn known_folder_path(id: &windows::core::GUID) -> Option<PathBuf> {
    unsafe {
        let raw = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let wide_slice = raw.as_wide();
        let path = PathBuf::from(OsString::from_wide(wide_slice));
        CoTaskMemFree(Some(raw.0.cast()));
        path.is_absolute().then_some(path)
    }
}

impl App {
    pub fn pick_bookmark(&self) {
        let mut entries = known_folders();
        let saved = self.saved_bookmarks.borrow().clone();
        for (index, bookmark) in saved.into_iter().enumerate() {
            entries.push(Entry {
                label: bookmark.name,
                path: bookmark.path,
                saved_index: Some(index),
            });
        }
        let state = Picker {
            entries: &entries,
            font: self.font.get(),
        };
        let active = self.active.get();
        let result = prompt::show_dialog(
            self.window,
            [305, 146],
            Some(picker_proc),
            LPARAM((&state as *const Picker) as isize),
        );
        match result {
            Ok(code) if code == SAVE as isize => self.save_bookmark(),
            Ok(code @ OPEN_FIRST..REMOVE_FIRST) => {
                if let Some(entry) = entries.get((code - OPEN_FIRST) as usize) {
                    self.navigate(active, entry.path.clone());
                }
            }
            Ok(code @ REMOVE_FIRST..) => {
                if let Some(saved_index) = entries
                    .get((code - REMOVE_FIRST) as usize)
                    .and_then(|entry| entry.saved_index)
                {
                    self.remove_bookmark(saved_index);
                }
            }
            Ok(_) => {}
            Err(error) => {
                message(
                    self.window,
                    "Could not open bookmarks",
                    &error.message(),
                    MB_OK | MB_ICONWARNING,
                );
            }
        }
        self.focus_list(active);
    }

    pub fn save_bookmark(&self) {
        let active = self.active.get();
        let (in_archive, folder) = {
            let listing = self.panes[active].listing.borrow();
            (listing.archive.is_some(), listing.path.clone())
        };
        if in_archive {
            message(
                self.window,
                "Save bookmark",
                "Open a disk folder to save a bookmark.",
                MB_OK | MB_ICONINFORMATION,
            );
            return;
        }
        if !folder.is_absolute() {
            return;
        }
        let default_name = folder
            .file_name()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| folder.as_os_str());
        let select_end = default_name.encode_wide().count();
        let name = prompt::name(
            self.window,
            self.font.get(),
            w!("Save bookmark"),
            default_name,
            select_end,
        );
        self.focus_list(active);
        let Some(name) = name else {
            return;
        };
        let label = name.encode_wide().collect::<Vec<_>>();
        let found = self.saved_bookmarks.borrow().iter().position(|bookmark| {
            let existing = bookmark.name.encode_wide().collect::<Vec<_>>();
            unsafe { CompareStringOrdinal(&existing, &label, true) == CSTR_EQUAL }
        });
        let bookmark = Bookmark { name, path: folder };
        if let Some(index) = found {
            let text = format!(
                "Replace the bookmark “{}” with this folder?",
                bookmark.name.to_string_lossy()
            );
            if message(
                self.window,
                "Replace bookmark",
                &text,
                MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
            ) != IDYES
            {
                return;
            }
            self.saved_bookmarks.borrow_mut()[index] = bookmark;
        } else {
            let mut saved = self.saved_bookmarks.borrow_mut();
            if saved.len() == MAX_BOOKMARKS {
                drop(saved);
                message(
                    self.window,
                    "Bookmarks are full",
                    "Remove a saved bookmark before adding another.",
                    MB_OK | MB_ICONINFORMATION,
                );
                return;
            }
            saved.push(bookmark);
        }
        if let Err(error) = self.save_workspace() {
            message(
                self.window,
                "Could not save workspace",
                &error.message(),
                MB_OK | MB_ICONWARNING,
            );
        }
    }

    fn remove_bookmark(&self, index: usize) {
        let name = self
            .saved_bookmarks
            .borrow()
            .get(index)
            .map(|bookmark| bookmark.name.clone());
        if let Some(name) = name {
            let text = format!("Remove the bookmark “{}”?", name.to_string_lossy());
            if message(
                self.window,
                "Remove bookmark",
                &text,
                MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
            ) == IDYES
            {
                self.saved_bookmarks.borrow_mut().remove(index);
                if let Err(error) = self.save_workspace() {
                    message(
                        self.window,
                        "Could not save workspace",
                        &error.message(),
                        MB_OK | MB_ICONWARNING,
                    );
                }
            }
        }
    }
}

fn initialize(window: HWND, state: &Picker) -> Result<()> {
    unsafe {
        SetWindowTextW(window, w!("Bookmarks & Known Folders"))?;
    }
    let normal = WINDOW_EX_STYLE::default();
    let edit = WINDOW_STYLE(ES_READONLY as u32 | ES_AUTOHSCROLL as u32);
    let controls = [
        (
            w!("Static"),
            w!("Jump active pane to folder"),
            normal,
            WINDOW_STYLE(0),
            0,
            [9, 8, 296, 19],
        ),
        (
            w!("ListBox"),
            w!(""),
            WS_EX_CLIENTEDGE,
            WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(LBS_NOTIFY as u32 | LBS_NOINTEGRALHEIGHT as u32),
            LIST,
            [9, 21, 296, 98],
        ),
        (
            w!("Static"),
            w!("Path"),
            normal,
            WINDOW_STYLE(0),
            0,
            [9, 104, 37, 115],
        ),
        (
            w!("Edit"),
            w!(""),
            WS_EX_CLIENTEDGE,
            edit,
            PATH_PREVIEW,
            [38, 102, 296, 116],
        ),
        (
            w!("Button"),
            w!("&Save current…"),
            normal,
            WS_TABSTOP,
            SAVE,
            [9, 122, 88, 138],
        ),
        (
            w!("Button"),
            w!("&Remove…"),
            normal,
            WS_TABSTOP,
            REMOVE,
            [93, 122, 149, 138],
        ),
        (
            w!("Button"),
            w!("&Open"),
            normal,
            WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            IDOK.0 as u16,
            [188, 122, 240, 138],
        ),
        (
            w!("Button"),
            w!("Cancel"),
            normal,
            WS_TABSTOP,
            IDCANCEL.0 as u16,
            [244, 122, 296, 138],
        ),
    ];
    for control in controls {
        prompt::add_control(window, state.font, control)?;
    }
    let list = unsafe { GetDlgItem(Some(window), LIST as i32)? };
    for entry in state.entries {
        let display = if entry.saved_index.is_some() {
            wide(&entry.label)
        } else {
            wide(format!("{}  (Known folder)", entry.label.to_string_lossy()))
        };
        send(list, LB_ADDSTRING, 0, display.as_ptr() as isize);
    }
    let default = if state.entries.is_empty() {
        SAVE
    } else {
        IDOK.0 as u16
    };
    send(window, DM_SETDEFID, default as usize, 0);
    let focus = if state.entries.is_empty() {
        unsafe { GetDlgItem(Some(window), SAVE as i32)? }
    } else {
        send(list, LB_SETCURSEL, 0, 0);
        list
    };
    update_preview(window, state);
    unsafe {
        let _ = SetFocus(Some(focus));
    }
    Ok(())
}

fn selected(window: HWND) -> Option<usize> {
    let list = unsafe { GetDlgItem(Some(window), LIST as i32) }.ok()?;
    usize::try_from(send(list, LB_GETCURSEL, 0, 0)).ok()
}

fn update_preview(window: HWND, state: &Picker) {
    let entry = selected(window).and_then(|index| state.entries.get(index));
    unsafe {
        if let Ok(button) = GetDlgItem(Some(window), IDOK.0) {
            let _ = EnableWindow(button, entry.is_some());
        }
        if let Ok(button) = GetDlgItem(Some(window), REMOVE as i32) {
            let _ = EnableWindow(button, entry.is_some_and(|item| item.saved_index.is_some()));
        }
    }
    if let Some(entry) = entry
        && let Ok(control) = unsafe { GetDlgItem(Some(window), PATH_PREVIEW as i32) }
    {
        show_path(control, &entry.path);
    }
}

unsafe extern "system" fn picker_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    unsafe {
        match message {
            WM_INITDIALOG => {
                SetWindowLongPtrW(window, GWLP_USERDATA, lparam.0);
                let state = &*(lparam.0 as *const Picker);
                if initialize(window, state).is_err() {
                    let _ = EndDialog(window, -1);
                }
                return 0;
            }
            WM_CLOSE => {
                let _ = EndDialog(window, IDCANCEL.0 as isize);
                return 1;
            }
            WM_COMMAND => {
                let command = wparam.0 as u16;
                let notification = (wparam.0 >> 16) as u16;
                let result = match command {
                    SAVE => Some(SAVE as isize),
                    REMOVE => selected(window).map(|index| REMOVE_FIRST + index as isize),
                    id if id == IDOK.0 as u16
                        || (id == LIST && notification == LBN_DBLCLK as u16) =>
                    {
                        selected(window).map(|index| OPEN_FIRST + index as isize)
                    }
                    id if id == IDCANCEL.0 as u16 => Some(IDCANCEL.0 as isize),
                    LIST if notification == LBN_SELCHANGE as u16 => {
                        let state = &*(GetWindowLongPtrW(window, GWLP_USERDATA) as *const Picker);
                        update_preview(window, state);
                        None
                    }
                    _ => return 0,
                };
                if let Some(result) = result {
                    let _ = EndDialog(window, result);
                }
                return 1;
            }
            _ => {}
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_windows_known_folders() {
        let folders = known_folders();
        assert!(!folders.is_empty());
        assert!(folders.iter().all(|entry| entry.path.is_absolute()));
    }
}
