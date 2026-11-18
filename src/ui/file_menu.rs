use super::*;
use crate::update;

pub(super) const SETTINGS: u16 = 191;
pub(super) const UPDATE: u16 = 192;
pub(super) const ABOUT: u16 = 193;

pub(super) fn attach(window: HWND) -> Result<()> {
    unsafe {
        let bar = CreateMenu()?;
        let result = (|| {
            let file = CreatePopupMenu()?;
            if let Err(error) = AppendMenuW(bar, MF_POPUP, file.0 as usize, w!("&File")) {
                let _ = DestroyMenu(file);
                return Err(error);
            }
            menu_label(file, SETTINGS, w!("&Settings…"))?;
            menu_label(file, UPDATE, w!("&Update…"))?;
            menu_label(file, ABOUT, w!("&About"))?;
            SetMenu(window, Some(bar))
        })();
        if result.is_err() {
            let _ = DestroyMenu(bar);
        }
        result
    }
}

impl App {
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
