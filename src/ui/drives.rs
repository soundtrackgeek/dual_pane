use super::*;
use std::path::{Component, Prefix};
use windows::Win32::Storage::FileSystem::GetLogicalDrives;

const DRIVE_COUNT: u16 = 26;
const DRIVE_FIRST: u16 = 700;
const DRIVE_END: u16 = DRIVE_FIRST + 2 * DRIVE_COUNT - 1;
pub(super) const MORE_DRIVES: u16 = 752;
pub(super) const RIGHT_MORE_DRIVES: u16 = MORE_DRIVES + 1;
pub(super) const DRIVE_MENU: u16 = 190;
pub(super) const DRIVE_ROW_HEIGHT: i32 = 30;

fn drive_letters(mask: u32) -> impl Iterator<Item = u16> {
    (0..DRIVE_COUNT).filter(move |drive| mask & (1 << drive) != 0)
}

fn drive_command(pane: usize, drive: u16) -> u16 {
    DRIVE_FIRST + pane as u16 * DRIVE_COUNT + drive
}

fn drive_letter(drive: u16) -> char {
    char::from(b'A' + drive as u8)
}

fn path_drive(path: &Path) -> Option<u16> {
    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return None;
    };
    match prefix.kind() {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
            Some(u16::from(letter.to_ascii_uppercase() - b'A'))
        }
        _ => None,
    }
}

pub(super) struct DriveStrip {
    buttons: RefCell<Vec<(u16, HWND)>>,
    more: HWND,
    overflow: Cell<bool>,
    current: Cell<Option<u16>>,
}

impl DriveStrip {
    pub fn new(window: HWND, index: usize) -> Result<Self> {
        Ok(Self {
            buttons: RefCell::default(),
            more: drive_button(window, w!("Drives…"), MORE_DRIVES + index as u16)?,
            overflow: Cell::default(),
            current: Cell::default(),
        })
    }

    pub fn font(&self, font: HFONT) {
        send(self.more, WM_SETFONT, font.0 as usize, 0);
        for &(_, control) in self.buttons.borrow().iter() {
            send(control, WM_SETFONT, font.0 as usize, 0);
        }
    }

    pub fn set_path(&self, path: &Path) {
        let current = path_drive(path);
        let previous = self.current.replace(current);
        if previous != current {
            for &(drive, control) in self.buttons.borrow().iter() {
                if Some(drive) == previous || Some(drive) == current {
                    unsafe {
                        let _ = InvalidateRect(Some(control), None, false);
                    }
                }
            }
        }
    }

    pub fn layout(&self, app: &App, bounds: RECT) {
        let width = bounds.right - bounds.left;
        let top = bounds.top + app.scale(59);
        let height = app.scale(24);
        let step = app.scale(34);
        let gap = app.scale(4);
        let more = app.scale(71);
        let buttons = self.buttons.borrow();
        let overflow = (buttons.len() as i32 * step - gap) > width;
        self.overflow.set(overflow);
        let available = if overflow { width - more } else { width + gap };
        let slots = (available / step).max(0) as usize;
        for (position, &(_, control)) in buttons.iter().enumerate() {
            if position < slots {
                place_drive(
                    control,
                    bounds.left + position as i32 * step,
                    top,
                    step - gap,
                    height,
                );
            }
            show(control, position < slots);
        }
        if overflow {
            let visible = slots.min(buttons.len()) as i32;
            place_drive(
                self.more,
                bounds.left + visible * step,
                top,
                more.min(width),
                height,
            );
        }
        show(self.more, overflow);
    }
}

fn drive_button(window: HWND, label: PCWSTR, id: u16) -> Result<HWND> {
    child(
        window,
        w!("Button"),
        label,
        WS_CLIPSIBLINGS | WINDOW_STYLE(BS_OWNERDRAW as u32),
        id,
    )
}

fn place_drive(window: HWND, x: i32, y: i32, width: i32, height: i32) {
    unsafe {
        // During a divider move, a button can cross the old position of another
        // button. Repaint from its own label instead of copying existing pixels.
        let _ = SetWindowPos(
            window,
            None,
            x,
            y,
            width.max(0),
            height,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOCOPYBITS,
        );
    }
}

fn show(window: HWND, visible: bool) {
    unsafe {
        // Avoid dispatching visibility changes again on each resize or divider movement.
        let shown = GetWindowLongW(window, GWL_STYLE) as u32 & WS_VISIBLE.0 != 0;
        if shown != visible {
            let _ = ShowWindow(window, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }
    }
}

impl App {
    pub fn refresh_drives(&self) {
        self.set_drive_mask(unsafe { GetLogicalDrives() });
    }

    pub fn set_drive_mask(&self, mask: u32) {
        if self.drive_mask.replace(Some(mask)) == Some(mask) {
            return;
        }
        for (index, pane) in self.panes.iter().enumerate() {
            let mut buttons = pane.drives.buttons.borrow_mut();
            buttons.retain(|&(drive, control)| {
                let present = mask & (1 << drive) != 0;
                if !present {
                    unsafe {
                        let _ = DestroyWindow(control);
                    }
                }
                present
            });
            for drive in drive_letters(mask) {
                if buttons.iter().any(|&(existing, _)| existing == drive) {
                    continue;
                }
                let label = [u16::from(b'A') + drive, u16::from(b':'), 0];
                if let Ok(control) = drive_button(
                    self.window,
                    PCWSTR(label.as_ptr()),
                    drive_command(index, drive),
                ) {
                    send(control, WM_SETFONT, self.font.get().0 as usize, 0);
                    buttons.push((drive, control));
                }
            }
            buttons.sort_unstable_by_key(|&(drive, _)| drive);
        }
        self.layout();
    }

    pub fn is_current_drive(&self, command: u16) -> bool {
        (DRIVE_FIRST..=DRIVE_END).contains(&command) && {
            let offset = command - DRIVE_FIRST;
            self.panes[usize::from(offset / DRIVE_COUNT)]
                .drives
                .current
                .get()
                == Some(offset % DRIVE_COUNT)
        }
    }

    pub fn drive_button_command(&self, command: u16) -> bool {
        match command {
            DRIVE_FIRST..=DRIVE_END => {
                let offset = command - DRIVE_FIRST;
                self.open_drive(usize::from(offset / DRIVE_COUNT), offset % DRIVE_COUNT);
            }
            MORE_DRIVES..=RIGHT_MORE_DRIVES => self.drive_menu(usize::from(command - MORE_DRIVES)),
            DRIVE_MENU => self.drive_menu(self.active.get()),
            _ => return false,
        }
        true
    }

    fn open_drive(&self, index: usize, drive: u16) {
        self.navigate(index, PathBuf::from(format!("{}:\\", drive_letter(drive))));
        self.focus_list(index);
    }

    pub fn drive_menu(&self, index: usize) {
        self.refresh_drives();
        let pane = &self.panes[index];
        let mut anchor = RECT::default();
        unsafe {
            let button = if pane.drives.overflow.get() {
                pane.drives.more
            } else {
                pane.tabs
            };
            if GetWindowRect(button, &mut anchor).is_err() {
                return;
            }
            let Ok(menu) = CreatePopupMenu() else { return };
            let mask = self.drive_mask.get().unwrap_or_default();
            for drive in drive_letters(mask) {
                let letter = drive_letter(drive);
                let text = wide(format!("&{letter}:\\"));
                let flags = MF_STRING
                    | if pane.drives.current.get() == Some(drive) {
                        MF_CHECKED
                    } else {
                        MF_UNCHECKED
                    };
                let _ = AppendMenuW(
                    menu,
                    flags,
                    usize::from(drive_command(index, drive)),
                    PCWSTR(text.as_ptr()),
                );
            }
            if mask == 0 {
                let _ = AppendMenuW(menu, MF_GRAYED, 0, w!("No drive letters available"));
            }
            let selected = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY,
                anchor.left,
                anchor.bottom,
                None,
                self.window,
                None,
            )
            .0 as u16;
            let _ = DestroyMenu(menu);
            if selected != 0 {
                self.open_drive(index, selected - drive_command(index, 0));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "system" fn record_positions(
        control: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _: usize,
        data: usize,
    ) -> LRESULT {
        unsafe {
            if message == WM_WINDOWPOSCHANGING {
                let position = &*(lparam.0 as *const WINDOWPOS);
                if !position.flags.contains(SWP_NOMOVE) {
                    (*(data as *const Cell<Option<SET_WINDOW_POS_FLAGS>>))
                        .set(Some(position.flags));
                }
            }
            DefSubclassProc(control, message, wparam, lparam)
        }
    }

    #[test]
    fn moving_the_right_drive_row_at_125_percent_discards_old_pixels() -> Result<()> {
        let _apartment = shell::Apartment::enter()?;
        let window = create_window()?;
        let app = attach(window, &Workspace::default())?;
        // C: and N: through Z: reproduce the two long drive rows.
        app.set_drive_mask((1 << 2) | (((1 << 13) - 1) << 13));
        let target = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        unsafe {
            SendMessageW(
                window,
                WM_DPICHANGED,
                Some(WPARAM(120 | (120 << 16))),
                Some(LPARAM((&target as *const RECT) as isize)),
            );
        }
        let right = app.panes[1].drives.buttons.borrow();
        let (_, control) = right[1];
        drop(right);
        let mut origin = POINT::default();
        unsafe { ClientToScreen(window, &mut origin).ok()? };
        for split in [0.62, 0.38, 0.50] {
            let last_flags = Cell::<Option<SET_WINDOW_POS_FLAGS>>::new(None);
            unsafe {
                SetWindowSubclass(
                    control,
                    Some(record_positions),
                    1,
                    (&last_flags as *const _) as usize,
                )
                .ok()?;
            }
            app.split.set(split);
            app.layout();
            unsafe { RemoveWindowSubclass(control, Some(record_positions), 1).ok()? };
            assert!(
                last_flags
                    .get()
                    .is_some_and(|flags| flags.contains(SWP_NOCOPYBITS))
            );
            let bounds = app.layout.get().panes[1];
            let mut previous = bounds.left;
            let right = &app.panes[1].drives;
            for control in right
                .buttons
                .borrow()
                .iter()
                .map(|entry| entry.1)
                .chain([right.more])
            {
                unsafe {
                    let style = GetWindowLongW(control, GWL_STYLE) as u32;
                    if style & WS_VISIBLE.0 != 0 {
                        assert_ne!(style & WS_CLIPSIBLINGS.0, 0);
                        let mut rect = RECT::default();
                        GetWindowRect(control, &mut rect)?;
                        assert!(rect.left - origin.x >= previous);
                        assert!(rect.right - origin.x <= bounds.right);
                        previous = rect.right - origin.x;
                    }
                }
            }
        }
        unsafe { DestroyWindow(window)? };
        Ok(())
    }
}
