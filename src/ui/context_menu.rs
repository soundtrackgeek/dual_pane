use super::*;
use crate::file_data;

pub(super) const OPEN_ITEM: u16 = 194;
pub(super) const COPY_PATH: u16 = 195;
pub(super) const OPEN_TERMINAL: u16 = 196;
pub(super) const OPEN_EXPLORER: u16 = 197;
pub(super) const PROPERTIES: u16 = 198;

impl App {
    pub fn context_menu_command(&self, command: u16) -> bool {
        match command {
            OPEN_ITEM => {
                let active = self.active.get();
                let focused = send(
                    self.panes[active].list,
                    LVM_GETNEXTITEM,
                    usize::MAX,
                    LVNI_FOCUSED as isize,
                );
                if focused >= 0 {
                    self.open_row(active, focused);
                }
            }
            COPY_PATH => self.copy_selected_paths(),
            OPEN_TERMINAL => self.open_in_terminal(),
            OPEN_EXPLORER => self.open_in_explorer(),
            PROPERTIES => self.show_properties(),
            _ => return false,
        }
        true
    }

    pub fn show_list_context_menu(&self, control: HWND, lparam: LPARAM) -> bool {
        let Some(index) = self.panes.iter().position(|pane| pane.list == control) else {
            return false;
        };
        self.active(index);
        let pane = &self.panes[index];
        let keyboard = lparam.0 == -1;
        let screen = if keyboard {
            self.keyboard_menu_anchor(pane.list)
        } else {
            point(lparam)
        };
        let on_item = if keyboard {
            pane.count() > 0
        } else {
            self.hit_test_item(pane.list, screen).is_some()
        };
        let selected = if on_item { pane.count() } else { 0 };
        let focused = send(
            pane.list,
            LVM_GETNEXTITEM,
            usize::MAX,
            LVNI_FOCUSED as isize,
        );
        let focused_item = (selected > 0 && focused >= 0)
            .then(|| pane.item(focused))
            .flatten();

        let (in_archive, disk_ready, ready, can_transfer, can_extract) = {
            let origin = pane.listing.borrow();
            let target = self.panes[1 - index].listing.borrow();
            let transfer_target = origin.ready()
                && self.can_queue_transfer()
                && target.complete
                && target.error.is_none()
                && target.archive.is_none()
                && origin.path != target.path
                && selected > 0;
            (
                origin.archive.is_some(),
                !self.operation_active.get() && origin.disk_ready(),
                origin.ready(),
                transfer_target && origin.archive.is_none(),
                transfer_target && origin.archive.is_some(),
            )
        };

        unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                return true;
            };
            if in_archive {
                if on_item && selected > 0 {
                    let can_open = focused_item.as_ref().is_some_and(|(_, dir)| *dir);
                    append_item(menu, OPEN_ITEM, w!("&Open\tEnter"), can_open);
                    append_item(menu, COPY, w!("E&xtract to opposite pane\tF5"), can_extract);
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                }
                let copy_label = if selected > 1 {
                    w!("Copy pat&hs\tShift+Alt+C")
                } else if selected == 1 {
                    w!("Copy pat&h\tShift+Alt+C")
                } else {
                    w!("Copy archive pat&h\tShift+Alt+C")
                };
                append_item(menu, COPY_PATH, copy_label, ready);
                append_item(menu, REFRESH, w!("&Refresh\tCtrl+R"), true);
            } else if on_item && selected > 0 {
                let can_open = focused_item.is_some();
                let can_quick_view = focused_item.as_ref().is_some_and(|(_, dir)| !*dir);
                append_item(menu, OPEN_ITEM, w!("&Open\tEnter"), can_open);
                append_item(menu, QUICK_VIEW, w!("&Quick View…\tF3"), can_quick_view);
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                append_item(menu, COPY, w!("Cop&y to opposite pane\tF5"), can_transfer);
                append_item(menu, MOVE, w!("&Move to opposite pane\tF6"), can_transfer);
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                append_item(menu, CLIP_CUT, w!("Cu&t\tCtrl+X"), disk_ready);
                append_item(menu, CLIP_COPY, w!("&Copy\tCtrl+C"), disk_ready);
                append_item(menu, CLIP_PASTE, w!("&Paste\tCtrl+V"), disk_ready);
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                append_item(
                    menu,
                    RENAME,
                    w!("Re&name…\tF2"),
                    disk_ready && selected == 1,
                );
                append_item(menu, BATCH_RENAME, w!("&Batch Rename…\tCtrl+M"), disk_ready);
                append_item(menu, RECYCLE, w!("&Recycle\tDel"), disk_ready);
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                let copy_label = if selected > 1 {
                    w!("Copy pat&hs\tShift+Alt+C")
                } else {
                    w!("Copy pat&h\tShift+Alt+C")
                };
                append_item(menu, COPY_PATH, copy_label, ready);
                append_item(menu, OPEN_EXPLORER, w!("Open in File &Explorer"), ready);
                append_item(menu, OPEN_TERMINAL, w!("Open in &Terminal\tAlt+T"), ready);
                append_item(
                    menu,
                    PROPERTIES,
                    w!("Pr&operties\tAlt+Enter"),
                    ready && selected == 1,
                );
            } else {
                append_item(menu, CLIP_PASTE, w!("&Paste\tCtrl+V"), disk_ready);
                append_item(menu, CREATE_FOLDER, w!("&New folder…\tF7"), disk_ready);
                append_item(menu, REFRESH, w!("&Refresh\tCtrl+R"), true);
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                append_item(menu, COPY_PATH, w!("Copy folder pat&h\tShift+Alt+C"), ready);
                append_item(menu, OPEN_EXPLORER, w!("Open in File &Explorer"), ready);
                append_item(menu, OPEN_TERMINAL, w!("Open in &Terminal\tAlt+T"), ready);
                append_item(menu, PROPERTIES, w!("Pr&operties\tAlt+Enter"), ready);
            }
            let _ = TrackPopupMenu(
                menu,
                TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RIGHTBUTTON,
                screen.x,
                screen.y,
                Some(0),
                self.window,
                None,
            );
            let _ = DestroyMenu(menu);
        }
        true
    }

    fn keyboard_menu_anchor(&self, list: HWND) -> POINT {
        let focused = send(list, LVM_GETNEXTITEM, usize::MAX, LVNI_FOCUSED as isize);
        let mut client = POINT {
            x: self.scale(16),
            y: self.scale(28),
        };
        if focused >= 0 {
            let mut rect = RECT {
                left: LVIR_LABEL as i32,
                ..Default::default()
            };
            if send(
                list,
                LVM_GETITEMRECT,
                focused as usize,
                (&mut rect as *mut RECT) as isize,
            ) != 0
            {
                client = POINT {
                    x: rect.left + self.scale(12),
                    y: rect.bottom,
                };
            }
        }
        unsafe {
            let _ = ClientToScreen(list, &mut client);
        }
        client
    }

    fn hit_test_item(&self, list: HWND, screen: POINT) -> Option<usize> {
        let mut client = screen;
        unsafe {
            let _ = ScreenToClient(list, &mut client);
        }
        let mut hit = LVHITTESTINFO {
            pt: client,
            ..Default::default()
        };
        let row = send(
            list,
            LVM_HITTEST,
            0,
            (&mut hit as *mut LVHITTESTINFO) as isize,
        );
        usize::try_from(row).ok()
    }

    pub fn copy_selected_paths(&self) {
        let index = self.active.get();
        let pane = &self.panes[index];
        let paths = {
            let listing = pane.listing.borrow();
            if !listing.ready() {
                return;
            }
            if let Some(folder) = &listing.archive {
                let selected: Vec<PathBuf> = pane
                    .selection()
                    .filter_map(|row| listing.rows.get(row))
                    .map(|row| archive_display_path(&listing.path, folder, Some(&row.filename())))
                    .collect();
                if selected.is_empty() {
                    vec![archive_display_path(&listing.path, folder, None)]
                } else {
                    selected
                }
            } else {
                let (folder, selected) = pane.selected_paths();
                if selected.is_empty() {
                    vec![folder]
                } else {
                    selected
                }
            }
        };
        let text = file_data::format_paths_text(&paths);
        match file_data::clipboard_write_text(self.window, &text) {
            Ok(_) => {
                *self.operation_status.borrow_mut() = if paths.len() == 1 {
                    "Copied path to clipboard".into()
                } else {
                    format!("Copied {} paths to clipboard", grouped(paths.len() as u64))
                };
                self.status_changed();
            }
            Err(error) => {
                message(
                    self.window,
                    "Could not copy path",
                    &error.message(),
                    MB_OK | MB_ICONWARNING,
                );
            }
        }
    }

    pub fn open_in_terminal(&self) {
        let pane = &self.panes[self.active.get()];
        let target = {
            let listing = pane.listing.borrow();
            if !listing.disk_ready() {
                return;
            }
            if pane.count() == 1
                && let Some((path, true)) = pane
                    .selection()
                    .next()
                    .and_then(|row| pane.item(row as isize))
            {
                path
            } else {
                listing.path.clone()
            }
        };
        if let Err(error) = shell::open_terminal(self.window, &target) {
            message(
                self.window,
                "Could not open terminal",
                &error.message(),
                MB_OK | MB_ICONWARNING,
            );
        }
    }

    pub fn open_in_explorer(&self) {
        let pane = &self.panes[self.active.get()];
        let (target, select) = {
            let listing = pane.listing.borrow();
            if !listing.disk_ready() {
                return;
            }
            let focused = send(
                pane.list,
                LVM_GETNEXTITEM,
                usize::MAX,
                LVNI_FOCUSED as isize,
            );
            if pane.count() > 0
                && focused >= 0
                && let Some((path, _)) = pane.item(focused)
            {
                (path, true)
            } else {
                (listing.path.clone(), false)
            }
        };
        if let Err(error) = shell::open_in_explorer(self.window, &target, select) {
            message(
                self.window,
                "Could not open in File Explorer",
                &error.message(),
                MB_OK | MB_ICONWARNING,
            );
        }
    }

    pub fn show_properties(&self) {
        let pane = &self.panes[self.active.get()];
        let target = {
            let listing = pane.listing.borrow();
            if !listing.disk_ready() {
                return;
            }
            let selected = pane.count();
            if selected > 1 {
                return;
            }
            if selected == 1 {
                let Some((path, _)) = pane
                    .selection()
                    .next()
                    .and_then(|row| pane.item(row as isize))
                else {
                    return;
                };
                path
            } else {
                listing.path.clone()
            }
        };
        if let Err(error) = shell::properties(self.window, &target) {
            message(
                self.window,
                "Could not open properties",
                &error.message(),
                MB_OK | MB_ICONWARNING,
            );
        }
    }
}

fn append_item(menu: HMENU, id: u16, label: PCWSTR, enabled: bool) {
    let flags = MF_STRING | if enabled { MF_ENABLED } else { MF_GRAYED };
    unsafe {
        let _ = AppendMenuW(menu, flags, usize::from(id), label);
    }
}

fn archive_display_path(archive: &Path, folder: &str, name: Option<&OsString>) -> PathBuf {
    let mut path = archive.to_path_buf();
    path.extend(folder.split('/').filter(|part| !part.is_empty()));
    if let Some(name) = name {
        path.push(name);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_archive_paths_for_copy_path() {
        let archive = Path::new(r"C:\Temp\bundle.zip");
        assert_eq!(
            archive_display_path(archive, "", None),
            PathBuf::from(r"C:\Temp\bundle.zip")
        );
        assert_eq!(
            archive_display_path(archive, "docs/sub", None),
            PathBuf::from(r"C:\Temp\bundle.zip\docs\sub")
        );
        assert_eq!(
            archive_display_path(archive, "docs", Some(&OsString::from("readme.txt"))),
            PathBuf::from(r"C:\Temp\bundle.zip\docs\readme.txt")
        );
    }

    #[test]
    fn formats_multiple_paths_with_crlf() {
        assert_eq!(
            file_data::format_paths_text(&[PathBuf::from(r"C:\solo.txt")]),
            OsString::from(r"C:\solo.txt")
        );
        let paths = [
            PathBuf::from(r"C:\a.txt"),
            PathBuf::from(r"C:\påske\🦀.txt"),
        ];
        assert_eq!(
            file_data::format_paths_text(&paths),
            OsString::from("C:\\a.txt\r\nC:\\påske\\🦀.txt")
        );
    }
}
