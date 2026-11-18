use super::*;
use crate::update;

pub(super) const SETTINGS: u16 = 191;
pub(super) const UPDATE: u16 = 192;
pub(super) const ABOUT: u16 = 193;

const FILE_LABEL: PCWSTR = w!("&File");
const FILE_BAR_ITEM: usize = 1;
const FIRST_FILE_COMMAND: usize = 2;
const FILE_ITEMS: [(u16, PCWSTR); 3] = [
    (SETTINGS, w!("&Settings…")),
    (UPDATE, w!("&Update…")),
    (ABOUT, w!("&About")),
];

pub(super) fn attach(window: HWND) -> Result<()> {
    replace_menu(window, HBRUSH::default(), false)
}

fn replace_menu(window: HWND, brush: HBRUSH, checking_update: bool) -> Result<()> {
    unsafe {
        let previous = GetMenu(window);
        let bar = CreateMenu()?;
        let result = (|| {
            let file = CreatePopupMenu()?;
            if let Err(error) = AppendMenuW(bar, MF_POPUP, file.0 as usize, FILE_LABEL) {
                let _ = DestroyMenu(file);
                return Err(error);
            }
            for (id, label) in FILE_ITEMS {
                menu_label(file, id, label)?;
            }
            if checking_update {
                let _ = EnableMenuItem(file, u32::from(UPDATE), MF_BYCOMMAND | MF_GRAYED);
            }
            if !brush.0.is_null() {
                let background = MENUINFO {
                    cbSize: size_of::<MENUINFO>() as u32,
                    fMask: MIM_BACKGROUND | MIM_APPLYTOSUBMENUS,
                    hbrBack: brush,
                    ..Default::default()
                };
                SetMenuInfo(bar, &background)?;
                owner_draw(bar, 0, FILE_BAR_ITEM)?;
                for index in 0..FILE_ITEMS.len() {
                    owner_draw(file, index as u32, FIRST_FILE_COMMAND + index)?;
                }
            }
            SetMenu(window, Some(bar))
        })();
        if result.is_err() {
            let _ = DestroyMenu(bar);
        } else if !previous.0.is_null() {
            let _ = DestroyMenu(previous);
        }
        result
    }
}

fn owner_draw(menu: HMENU, position: u32, data: usize) -> Result<()> {
    let item = MENUITEMINFOW {
        cbSize: size_of::<MENUITEMINFOW>() as u32,
        fMask: MIIM_FTYPE | MIIM_DATA,
        fType: MFT_OWNERDRAW,
        dwItemData: data,
        ..Default::default()
    };
    unsafe { SetMenuItemInfoW(menu, position, true, &item) }
}

fn item_label(data: usize) -> Option<PCWSTR> {
    if data == FILE_BAR_ITEM {
        Some(FILE_LABEL)
    } else {
        FILE_ITEMS
            .get(data.checked_sub(FIRST_FILE_COMMAND)?)
            .map(|item| item.1)
    }
}

impl App {
    pub fn theme_file_menu(&self, dpi_changed: bool) {
        let (dark, color) = {
            let palette = self.palette.borrow();
            (palette.dark, palette.surface)
        };
        let previous = self.file_menu_brush.get();
        let was_dark = !previous.0.is_null();
        if dark == was_dark && !(dark && dpi_changed) {
            return;
        }
        unsafe {
            let brush = if dark {
                CreateSolidBrush(color)
            } else {
                HBRUSH::default()
            };
            if dark && brush.0.is_null() {
                return;
            }
            // A fresh native menu refreshes cached item measurements and rendering.
            if replace_menu(self.window, brush, self.checking_update.get()).is_ok() {
                self.file_menu_brush.set(brush);
                let _ = DeleteObject(previous.into());
            } else {
                let _ = DeleteObject(brush.into());
            }
        }
    }

    pub fn measure_file_menu(&self, item: &mut MEASUREITEMSTRUCT) -> bool {
        let Some(label) = item_label(item.itemData) else {
            return false;
        };
        unsafe {
            let dc = GetDC(Some(self.window));
            let previous = SelectObject(dc, self.font.get().into());
            let mut bounds = RECT::default();
            DrawTextW(
                dc,
                &mut label.as_wide().to_vec(),
                &mut bounds,
                DT_SINGLELINE | DT_CALCRECT,
            );
            SelectObject(dc, previous);
            ReleaseDC(Some(self.window), dc);
            item.itemWidth = (bounds.right
                + self.scale(if item.itemData == FILE_BAR_ITEM {
                    12
                } else {
                    24
                })) as u32;
            item.itemHeight = (bounds.bottom + self.scale(6)) as u32;
        }
        true
    }

    pub fn draw_file_menu(&self, item: &DRAWITEMSTRUCT) -> bool {
        let Some(label) = item_label(item.itemData) else {
            return false;
        };
        let state = item.itemState;
        let disabled = state.0 & (ODS_DISABLED.0 | ODS_GRAYED.0) != 0;
        let selected = state.0 & (ODS_SELECTED.0 | ODS_HOTLIGHT.0) != 0;
        let palette = self.palette.borrow();
        let (background, foreground) = palette.menu_colors(selected, disabled);
        unsafe {
            let dc = item.hDC;
            let saved = SaveDC(dc);
            let mut bounds = item.rcItem;
            paint::background(dc, bounds, background);
            SelectObject(dc, self.font.get().into());
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, foreground);
            let mut format = DT_SINGLELINE | DT_VCENTER;
            if item.itemData == FILE_BAR_ITEM {
                format |= DT_CENTER;
            } else {
                bounds.left += self.scale(20);
            }
            if state.0 & ODS_NOACCEL.0 != 0 {
                format |= DT_HIDEPREFIX;
            }
            DrawTextW(dc, &mut label.as_wide().to_vec(), &mut bounds, format);
            let _ = RestoreDC(dc, saved);
        }
        true
    }

    pub fn file_menu_key(&self, key: u16, menu: HMENU) -> Option<LRESULT> {
        if self.file_menu_brush.get().0.is_null() {
            return None;
        }
        unsafe {
            let bar = GetMenu(self.window);
            let file = GetSubMenu(bar, 0);
            let key = u8::try_from(key).ok()?.to_ascii_lowercase();
            if menu == bar && key == b'f' {
                return Some(LRESULT((MNC_EXECUTE << 16) as isize));
            }
            if menu == file {
                let index = [b's', b'u', b'a']
                    .iter()
                    .position(|&letter| letter == key)?;
                let state = GetMenuState(file, index as u32, MF_BYPOSITION);
                if state & (MF_GRAYED.0 | MF_DISABLED.0) == 0 {
                    return Some(LRESULT((MNC_EXECUTE << 16) as isize | index as isize));
                }
            }
        }
        None
    }

    pub fn show_settings(&self) {
        let themes = [
            (SYSTEM, w!("System"), Theme::System),
            (LIGHT, w!("Light"), Theme::Light),
            (DARK, w!("Dark"), Theme::Dark),
        ];
        let choices = themes.map(|(id, label, _)| TASKDIALOG_BUTTON {
            nButtonID: i32::from(id),
            pszButtonText: label,
        });
        let mut selected = match self.theme.get() {
            Theme::System => SYSTEM,
            Theme::Light => LIGHT,
            Theme::Dark => DARK,
        }
        .into();
        let mut response = 0;
        let dialog = TASKDIALOGCONFIG {
            cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
            hwndParent: self.window,
            dwFlags: TDF_POSITION_RELATIVE_TO_WINDOW,
            dwCommonButtons: TDCBF_OK_BUTTON | TDCBF_CANCEL_BUTTON,
            pszWindowTitle: w!("Filehelm Settings"),
            pszMainInstruction: w!("Appearance"),
            pszContent: w!("Choose a theme for Filehelm."),
            cRadioButtons: choices.len() as u32,
            pRadioButtons: choices.as_ptr(),
            nDefaultRadioButton: selected,
            ..Default::default()
        };
        if let Err(error) =
            unsafe { TaskDialogIndirect(&dialog, Some(&mut response), Some(&mut selected), None) }
        {
            message(
                self.window,
                "Could not open Settings",
                &error.message(),
                MB_OK | MB_ICONWARNING,
            );
        } else if response == IDOK.0
            && let Some((_, _, theme)) = themes
                .into_iter()
                .find(|(id, _, _)| i32::from(*id) == selected)
        {
            self.theme.set(theme);
            self.apply_theme();
        }
    }

    pub fn show_about(&self) {
        message(
            self.window,
            "About Filehelm",
            &format!(
                "Filehelm {}\n\nKeyboard-first file manager for Windows 11.",
                update::CURRENT
            ),
            MB_OK | MB_ICONINFORMATION,
        );
    }

    pub fn start_update_check(&self) {
        if self.pending_update.borrow().is_some() {
            self.install_pending_update();
            return;
        }
        if self.checking_update.replace(true) {
            return;
        }
        self.update_note.set("Checking for updates…");
        self.update_progress();
        let sender = self.sender.clone();
        let owner = self.window.0 as usize;
        if let Err(error) = thread::Builder::new()
            .name("update-check".into())
            .spawn(move || {
                if sender.send(Event::UpdateChecked(update::check())).is_ok() {
                    wake(owner);
                }
            })
        {
            self.finish_update_check(Err(error.to_string()));
        }
    }

    fn update_progress(&self) {
        let flags = if self.checking_update.get() {
            MF_GRAYED
        } else {
            MF_ENABLED
        };
        unsafe {
            let bar = GetMenu(self.window);
            let file = GetSubMenu(bar, 0);
            let _ = EnableMenuItem(file, u32::from(UPDATE), MF_BYCOMMAND | flags);
        }
        self.status_changed();
    }

    pub fn finish_update_check(
        &self,
        result: std::result::Result<Option<update::Release>, String>,
    ) {
        self.checking_update.set(false);
        self.update_note.set("");
        self.update_progress();
        match result {
            Ok(Some(release)) => {
                if self.confirm_update(&release) {
                    self.download_update(release);
                }
            }
            Ok(None) => {
                message(
                    self.window,
                    "Filehelm is up to date",
                    &format!("You are running Filehelm {}.", update::CURRENT),
                    MB_OK | MB_ICONINFORMATION,
                );
            }
            Err(error) => self.offer_downloads(
                "Could not check for updates",
                &format!("{error}\n\nOpen the GitHub releases page instead?"),
                update::RELEASES,
                MB_ICONWARNING,
            ),
        }
    }

    fn confirm_update(&self, release: &update::Release) -> bool {
        let title = wide(format!("Filehelm {} is available", release.version));
        let content = wide(format!(
            "You are running Filehelm {}.\n\nFilehelm will download the update, save your workspace, and reopen after installation.",
            update::CURRENT
        ));
        let install = TASKDIALOG_BUTTON {
            nButtonID: IDYES.0,
            pszButtonText: w!("Install update"),
        };
        let dialog = TASKDIALOGCONFIG {
            cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
            hwndParent: self.window,
            dwFlags: TDF_POSITION_RELATIVE_TO_WINDOW,
            dwCommonButtons: TDCBF_CANCEL_BUTTON,
            pszWindowTitle: w!("Filehelm update available"),
            pszMainInstruction: PCWSTR(title.as_ptr()),
            pszContent: PCWSTR(content.as_ptr()),
            cButtons: 1,
            pButtons: &install,
            nDefaultButton: IDCANCEL.0,
            ..Default::default()
        };
        let mut choice = 0;
        match unsafe { TaskDialogIndirect(&dialog, Some(&mut choice), None, None) } {
            Ok(()) => choice == IDYES.0,
            Err(error) => {
                self.update_error(&error.message());
                false
            }
        }
    }

    fn download_update(&self, release: update::Release) {
        self.checking_update.set(true);
        self.update_note.set("Downloading update…");
        self.update_progress();
        let sender = self.sender.clone();
        let owner = self.window.0 as usize;
        if let Err(error) = thread::Builder::new()
            .name("update-download".into())
            .spawn(move || {
                if sender
                    .send(Event::UpdateDownloaded(release.download()))
                    .is_ok()
                {
                    wake(owner);
                }
            })
        {
            self.finish_update_download(Err(error.to_string()));
        }
    }

    pub fn finish_update_download(
        &self,
        result: std::result::Result<update::Prepared, String>,
    ) -> bool {
        self.checking_update.set(false);
        self.update_note.set("");
        self.update_progress();
        match result {
            Ok(update) => {
                self.pending_update.replace(Some(update));
                self.install_pending_update()
            }
            Err(error) => {
                self.update_error(&error);
                false
            }
        }
    }

    fn install_pending_update(&self) -> bool {
        if self.operation_active.get() || self.waiting_transfers() != 0 {
            self.update_note.set("Update ready");
            self.update_progress();
            message(
                self.window,
                "Filehelm update ready",
                "Finish active file operations and finish or remove queued transfers. Then choose File → Update again to install.",
                MB_OK | MB_ICONINFORMATION,
            );
            return false;
        }
        if let Err(error) = self.save_workspace() {
            self.update_error(&format!(
                "Could not save your workspace.\n\n{}",
                error.message()
            ));
            return false;
        }
        let Some(update) = self.pending_update.borrow_mut().take() else {
            return false;
        };
        let temporary_paths = (!self.persist).then(|| {
            self.panes
                .each_ref()
                .map(|pane| pane.listing.borrow().path.clone())
        });
        self.update_note.set("");
        self.update_progress();
        if let Err(error) = update.hand_off(temporary_paths) {
            self.update_error(&error);
            return false;
        }
        unsafe {
            let _ = DestroyWindow(self.window);
        }
        true
    }

    fn update_error(&self, error: &str) {
        message(
            self.window,
            "Could not update Filehelm",
            error,
            MB_OK | MB_ICONWARNING,
        );
    }

    fn offer_downloads(&self, title: &str, text: &str, url: &str, icon: MESSAGEBOX_STYLE) {
        if message(self.window, title, text, MB_YESNO | MB_DEFBUTTON2 | icon) == IDYES
            && let Err(error) = shell::open(self.window, Path::new(url))
        {
            message(
                self.window,
                "Could not open release downloads",
                &format!("{}\n\n{url}", error.message()),
                MB_OK | MB_ICONWARNING,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_theme_rebuilds_native_labels_and_preserves_disabled_update() -> Result<()> {
        let window = create_window()?;
        unsafe {
            let dark_brush = HBRUSH(GetStockObject(BLACK_BRUSH).0);
            let result = (|| {
                for brush in [dark_brush, HBRUSH::default(), dark_brush, HBRUSH::default()] {
                    let previous = GetMenu(window);
                    replace_menu(window, brush, true)?;
                    let bar = GetMenu(window);
                    assert_ne!(bar, previous);
                    let file = GetSubMenu(bar, 0);
                    for menu in [bar, file] {
                        let mut info = MENUINFO {
                            cbSize: size_of::<MENUINFO>() as u32,
                            fMask: MIM_BACKGROUND,
                            ..Default::default()
                        };
                        GetMenuInfo(menu, &mut info)?;
                        assert_eq!(info.hbrBack, brush);
                    }
                    for (menu, index, label) in std::iter::once((bar, 0, FILE_LABEL)).chain(
                        FILE_ITEMS
                            .iter()
                            .enumerate()
                            .map(|(index, &(_, label))| (file, index as u32, label)),
                    ) {
                        let mut info = MENUITEMINFOW {
                            cbSize: size_of::<MENUITEMINFOW>() as u32,
                            fMask: MIIM_FTYPE,
                            ..Default::default()
                        };
                        GetMenuItemInfoW(menu, index, true, &mut info)?;
                        assert_eq!(
                            info.fType,
                            if brush.0.is_null() {
                                MFT_STRING
                            } else {
                                MFT_OWNERDRAW
                            }
                        );
                        let mut text = [0; 32];
                        let length = GetMenuStringW(menu, index, Some(&mut text), MF_BYPOSITION);
                        assert_eq!(&text[..length as usize], label.as_wide());
                    }
                    assert_eq!(
                        GetMenuState(file, u32::from(UPDATE), MF_BYCOMMAND) & MF_GRAYED.0,
                        MF_GRAYED.0
                    );
                }
                Ok(())
            })();
            let _ = DestroyWindow(window);
            result
        }
    }
}
