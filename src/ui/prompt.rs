use super::*;

use std::{ffi::OsStr, os::windows::ffi::OsStrExt};

const INPUT: u16 = 171;

#[repr(C, align(4))]
struct Template {
    dialog: DLGTEMPLATE,
    menu_class_title: [u16; 3],
}

struct Prompt<'a> {
    title: PCWSTR,
    font: HFONT,
    original: &'a OsStr,
    select_end: usize,
    value: RefCell<Option<OsString>>,
}

pub fn name(
    owner: HWND,
    font: HFONT,
    title: PCWSTR,
    original: &OsStr,
    select_end: usize,
) -> Option<OsString> {
    let state = Prompt {
        title,
        font,
        original,
        select_end,
        value: RefCell::new(None),
    };
    if let Err(error) = show_dialog(
        owner,
        [244, 71],
        Some(dialog_proc),
        LPARAM((&state as *const Prompt) as isize),
    ) {
        message(
            owner,
            "Could not open name dialog",
            &error.message(),
            MB_OK | MB_ICONERROR,
        );
    }
    state.value.into_inner()
}

pub(super) fn show_dialog(
    owner: HWND,
    [cx, cy]: [i16; 2],
    procedure: DLGPROC,
    state: LPARAM,
) -> Result<isize> {
    let template = dialog_template([cx, cy]);
    let result = unsafe {
        let instance = HINSTANCE(GetModuleHandleW(None)?.0);
        DialogBoxIndirectParamW(
            Some(instance),
            &template.dialog,
            Some(owner),
            procedure,
            state,
        )
    };
    if result == -1 {
        return Err(windows::core::Error::from_thread());
    }
    Ok(result)
}

pub(super) fn create_panel(
    owner: HWND,
    size: [i16; 2],
    procedure: DLGPROC,
    state: LPARAM,
) -> Result<HWND> {
    let template = dialog_template(size);
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(None)?.0);
        CreateDialogIndirectParamW(
            Some(instance),
            &template.dialog,
            Some(owner),
            procedure,
            state,
        )
    }
}

fn dialog_template([cx, cy]: [i16; 2]) -> Template {
    Template {
        dialog: DLGTEMPLATE {
            style: (WS_POPUP | WS_CAPTION | WS_SYSMENU).0 | (DS_CENTER | DS_MODALFRAME) as u32,
            cx,
            cy,
            ..Default::default()
        },
        menu_class_title: [0; 3],
    }
}

pub(super) type Control = (PCWSTR, PCWSTR, WINDOW_EX_STYLE, WINDOW_STYLE, u16, [i32; 4]);

pub(super) fn add_control(dialog: HWND, font: HFONT, control: Control) -> Result<HWND> {
    let (class, text, ex_style, style, id, [left, top, right, bottom]) = control;
    let mut bounds = RECT {
        left,
        top,
        right,
        bottom,
    };
    unsafe {
        MapDialogRect(dialog, &mut bounds)?;
        let window = CreateWindowExW(
            ex_style,
            class,
            text,
            WS_CHILD | WS_VISIBLE | style,
            bounds.left,
            bounds.top,
            bounds.right - bounds.left,
            bounds.bottom - bounds.top,
            Some(dialog),
            Some(HMENU(id as usize as _)),
            None,
            None,
        )?;
        send(window, WM_SETFONT, font.0 as usize, 0);
        Ok(window)
    }
}

fn initialize(dialog: HWND, state: &Prompt) -> Result<()> {
    unsafe {
        SetWindowTextW(dialog, state.title)?;
    }
    let original = wide(state.original);
    let controls = [
        (
            w!("Static"),
            w!("Name"),
            WINDOW_EX_STYLE::default(),
            WINDOW_STYLE(0),
            170,
            [9, 8, 235, 19],
        ),
        (
            w!("Edit"),
            PCWSTR(original.as_ptr()),
            WS_EX_CLIENTEDGE,
            WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            INPUT,
            [9, 21, 235, 36],
        ),
        (
            w!("Button"),
            w!("OK"),
            WINDOW_EX_STYLE::default(),
            WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            IDOK.0 as u16,
            [125, 46, 177, 62],
        ),
        (
            w!("Button"),
            w!("Cancel"),
            WINDOW_EX_STYLE::default(),
            WS_TABSTOP,
            IDCANCEL.0 as u16,
            [183, 46, 235, 62],
        ),
    ];
    for control in controls {
        let window = add_control(dialog, state.font, control)?;
        if control.4 == INPUT {
            send(window, EM_SETLIMITTEXT, 255, 0);
            send(window, EM_SETSEL, 0, state.select_end as isize);
            unsafe {
                let _ = SetFocus(Some(window));
            }
        }
    }
    send(dialog, DM_SETDEFID, IDOK.0 as usize, 0);
    Ok(())
}

unsafe extern "system" fn dialog_proc(
    window: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    unsafe {
        match msg {
            WM_INITDIALOG => {
                let state = &*(lparam.0 as *const Prompt);
                SetWindowLongPtrW(window, GWLP_USERDATA, lparam.0);
                if initialize(window, state).is_err() {
                    let _ = EndDialog(window, -1);
                }
                // Focus has already been placed in the edit.
                return 0;
            }
            WM_COMMAND if wparam.0 as u16 == IDOK.0 as u16 => {
                let state = &*(GetWindowLongPtrW(window, GWLP_USERDATA) as *const Prompt);
                let Ok(edit) = GetDlgItem(Some(window), INPUT as i32) else {
                    return 0;
                };
                let value = window_text(edit);
                if let Some(error) = invalid_name(&value) {
                    message(window, "Invalid name", error, MB_OK | MB_ICONWARNING);
                    let _ = SetFocus(Some(edit));
                } else {
                    state.value.replace(Some(value));
                    let _ = EndDialog(window, IDOK.0 as isize);
                }
                return 1;
            }
            WM_CLOSE | WM_COMMAND if msg == WM_CLOSE || wparam.0 as u16 == IDCANCEL.0 as u16 => {
                let _ = EndDialog(window, IDCANCEL.0 as isize);
                return 1;
            }
            _ => {}
        }
    }
    0
}

fn invalid_name(value: &OsStr) -> Option<&'static str> {
    let units = value.encode_wide().collect::<Vec<_>>();
    crate::directory::filename_issue(&units)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_filename_components_without_modifying_unicode() {
        for item in [
            "",
            ".",
            "..",
            r"child\file",
            "C:name",
            "x?",
            "x.",
            "x ",
            "COM¹.txt",
            "con.txt",
            "LPT9",
        ] {
            assert!(invalid_name(OsStr::new(item)).is_some(), "{item}");
        }
        for item in [
            "æøå 日本語.txt",
            "package.tar.gz",
            ".gitignore",
            "com10.txt",
            "contains  space",
        ] {
            assert!(invalid_name(OsStr::new(item)).is_none(), "{item}");
        }
    }
}
