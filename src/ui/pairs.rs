use super::*;
use navigation::MAX_PAIRS;

const SAVE: u16 = 400;
const REMOVE: u16 = 401;
const LIST: u16 = 402;
const LEFT_PATH: u16 = 403;
const RIGHT_PATH: u16 = 404;
const OPEN_FIRST: isize = 410;
const REMOVE_FIRST: isize = 450;

struct Picker<'a> {
    pairs: &'a [SavedPair],
    font: HFONT,
}

impl App {
    pub fn pick_folder_pair(&self) {
        let pairs = self.saved_pairs.borrow().clone();
        let state = Picker {
            pairs: &pairs,
            font: self.font.get(),
        };
        let active = self.active.get();
        let result = prompt::show_dialog(
            self.window,
            [305, 158],
            Some(picker_proc),
            LPARAM((&state as *const Picker) as isize),
        );
        match result {
            Ok(code) if code == SAVE as isize => self.save_pair(),
            Ok(code @ OPEN_FIRST..REMOVE_FIRST) => {
                if let Some(pair) = pairs.get((code - OPEN_FIRST) as usize) {
                    let [left, right] = pair.paths.clone();
                    self.navigate(0, left);
                    self.navigate(1, right);
                }
            }
            Ok(code @ REMOVE_FIRST..) => self.remove_pair((code - REMOVE_FIRST) as usize),
            Ok(_) => {}
            Err(error) => {
                message(
                    self.window,
                    "Could not open folder pairs",
                    &error.message(),
                    MB_OK | MB_ICONWARNING,
                );
            }
        }
        self.focus_list(active);
    }
    pub fn save_pair(&self) {
        if self
            .panes
            .iter()
            .any(|pane| pane.listing.borrow().archive.is_some())
        {
            message(
                self.window,
                "Save folder pair",
                "Open two disk folders to save a pair.",
                MB_OK | MB_ICONINFORMATION,
            );
            return;
        }
        let active = self.active.get();
        let folders = self
            .panes
            .each_ref()
            .map(|pane| pane.listing.borrow().path.clone());
        if folders.iter().any(|path| !path.is_absolute()) {
            return;
        }
        let name = prompt::name(
            self.window,
            self.font.get(),
            w!("Save folder pair"),
            "Folder pair".as_ref(),
            11,
        );
        self.focus_list(active);
        let Some(name) = name else {
            return;
        };
        let label = name.encode_wide().collect::<Vec<_>>();
        let found = self.saved_pairs.borrow().iter().position(|pair| {
            let existing = pair.name.encode_wide().collect::<Vec<_>>();
            unsafe { CompareStringOrdinal(&existing, &label, true) == CSTR_EQUAL }
        });
        let pair = SavedPair {
            name,
            paths: folders,
        };
        if let Some(index) = found {
            let text = format!(
                "Replace the saved pair “{}” with these folders?",
                pair.name.to_string_lossy()
            );
            if message(
                self.window,
                "Replace folder pair",
                &text,
                MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
            ) != IDYES
            {
                return;
            }
            self.saved_pairs.borrow_mut()[index] = pair;
        } else {
            let mut saved = self.saved_pairs.borrow_mut();
            if saved.len() == MAX_PAIRS {
                drop(saved);
                message(
                    self.window,
                    "Saved pairs are full",
                    "Remove a saved pair before adding another.",
                    MB_OK | MB_ICONINFORMATION,
                );
                return;
            }
            saved.push(pair);
        }
        self.persist_pair_change();
    }

    fn remove_pair(&self, index: usize) {
        let name = self
            .saved_pairs
            .borrow()
            .get(index)
            .map(|pair| pair.name.clone());
        if let Some(name) = name {
            let text = format!("Remove the saved pair “{}”?", name.to_string_lossy());
            if message(
                self.window,
                "Remove folder pair",
                &text,
                MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
            ) == IDYES
            {
                self.saved_pairs.borrow_mut().remove(index);
                self.persist_pair_change();
            }
        }
    }

    fn persist_pair_change(&self) {
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

fn initialize(window: HWND, state: &Picker) -> Result<()> {
    unsafe {
        SetWindowTextW(window, w!("Folder pairs"))?;
    }
    let caption = if state.pairs.is_empty() {
        w!("No saved folder pairs")
    } else {
        w!("Saved folder pairs")
    };
    let normal = WINDOW_EX_STYLE::default();
    let edit = WINDOW_STYLE(ES_READONLY as u32 | ES_AUTOHSCROLL as u32);
    let controls = [
        (
            w!("Static"),
            caption,
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
            [9, 21, 296, 92],
        ),
        (
            w!("Static"),
            w!("Left"),
            normal,
            WINDOW_STYLE(0),
            0,
            [9, 98, 37, 109],
        ),
        (
            w!("Edit"),
            w!(""),
            WS_EX_CLIENTEDGE,
            edit,
            LEFT_PATH,
            [38, 96, 296, 110],
        ),
        (
            w!("Static"),
            w!("Right"),
            normal,
            WINDOW_STYLE(0),
            0,
            [9, 114, 37, 125],
        ),
        (
            w!("Edit"),
            w!(""),
            WS_EX_CLIENTEDGE,
            edit,
            RIGHT_PATH,
            [38, 112, 296, 126],
        ),
        (
            w!("Button"),
            w!("&Save current…"),
            normal,
            WS_TABSTOP,
            SAVE,
            [9, 133, 88, 149],
        ),
        (
            w!("Button"),
            w!("&Remove…"),
            normal,
            WS_TABSTOP,
            REMOVE,
            [93, 133, 149, 149],
        ),
        (
            w!("Button"),
            w!("&Open"),
            normal,
            WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            IDOK.0 as u16,
            [188, 133, 240, 149],
        ),
        (
            w!("Button"),
            w!("Cancel"),
            normal,
            WS_TABSTOP,
            IDCANCEL.0 as u16,
            [244, 133, 296, 149],
        ),
    ];
    for control in controls {
        prompt::add_control(window, state.font, control)?;
    }
    let list = unsafe { GetDlgItem(Some(window), LIST as i32)? };
    for pair in state.pairs {
        let text = wide(&pair.name);
        send(list, LB_ADDSTRING, 0, text.as_ptr() as isize);
    }
    let default = if state.pairs.is_empty() {
        SAVE
    } else {
        IDOK.0 as u16
    };
    send(window, DM_SETDEFID, default as usize, 0);
    let focus = if state.pairs.is_empty() {
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
    let pair = selected(window).and_then(|index| state.pairs.get(index));
    for id in [IDOK.0, REMOVE as i32] {
        unsafe {
            if let Ok(button) = GetDlgItem(Some(window), id) {
                let _ = EnableWindow(button, pair.is_some());
            }
        }
    }
    if let Some(pair) = pair {
        for (id, path) in [LEFT_PATH, RIGHT_PATH].into_iter().zip(&pair.paths) {
            if let Ok(control) = unsafe { GetDlgItem(Some(window), id as i32) } {
                show_path(control, path);
            }
        }
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
