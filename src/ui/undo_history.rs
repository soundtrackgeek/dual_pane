use super::*;
use crate::directory;
use crate::undo::{Applied, Check, Group};

const GROUPS: u16 = 681;
const ITEMS: u16 = 682;
const SUMMARY: u16 = 683;
const REFRESH: u16 = 684;
const APPLY: u16 = 685;
const CLOSE: u16 = IDCANCEL.0 as u16;

#[derive(Default)]
struct Model {
    groups: Vec<Arc<Group>>,
    selected: Option<usize>,
    checks: Vec<Check>,
    note: String,
}

impl Model {
    fn group(&self) -> Option<&Arc<Group>> {
        self.groups.get(self.selected?)
    }

    fn ready(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| **check == Check::Ready)
            .count()
    }
}

#[derive(Default)]
pub(super) struct History {
    pub window: Cell<HWND>,
    model: RefCell<Model>,
    generation: Cell<usize>,
    preferred: Cell<Option<u128>>,
    reading: Cell<bool>,
    applying: Cell<bool>,
    affected: RefCell<HashSet<PathBuf>>,
}

impl History {
    fn next(&self) -> usize {
        let id = self.generation.get().wrapping_add(1);
        self.generation.set(id);
        id
    }
}

impl App {
    pub fn show_undo_history(&self) {
        let history = &self.undo_history;
        let existing = history.window.get();
        if !existing.0.is_null() {
            unsafe {
                let _ = SetForegroundWindow(existing);
            }
            self.load_history(existing, None);
            return;
        }
        match prompt::create_panel(
            self.window,
            [525, 258],
            WINDOW_STYLE(0),
            Some(panel_proc),
            LPARAM((self as *const App) as isize),
        ) {
            Ok(window) if unsafe { IsWindow(Some(window)).as_bool() } => {
                history.window.set(window);
                self.load_history(window, None);
                unsafe {
                    let _ = ShowWindow(window, SW_SHOW);
                }
            }
            Ok(_) => {}
            Err(error) => {
                message(
                    self.window,
                    "Could not open Operation History",
                    &error.message(),
                    MB_OK | MB_ICONWARNING,
                );
            }
        }
    }

    fn load_history(&self, panel: HWND, preferred: Option<u128>) {
        let history = &self.undo_history;
        if history.applying.get() {
            return;
        }
        if history.reading.replace(true) {
            self.render_history(panel);
            return;
        }
        let id = history.next();
        history.preferred.set(preferred);
        history.model.replace(Model {
            note: "Loading operation history…".into(),
            ..Model::default()
        });
        send(control(panel, GROUPS), CB_RESETCONTENT, 0, 0);
        self.render_history(panel);
        let store = self.undo_store.clone();
        let sender = self.sender.clone();
        let owner = self.window.0 as usize;
        if let Err(error) = thread::Builder::new()
            .name("undo-history".into())
            .spawn(move || {
                if sender.send(Event::HistoryLoaded(id, store.list())).is_ok() {
                    wake(owner);
                }
            })
        {
            self.receive_undo_history(id, Err(error));
        }
    }

    pub fn receive_undo_history(&self, id: usize, groups: std::io::Result<Vec<Arc<Group>>>) {
        let history = &self.undo_history;
        history.reading.set(false);
        let panel = history.window.get();
        if panel.0.is_null() {
            return;
        }
        if id != history.generation.get() {
            self.load_history(panel, None);
            return;
        }
        let groups = match groups {
            Ok(groups) => groups,
            Err(error) => {
                history.model.borrow_mut().note = format!("Could not load history · {error}");
                self.render_history(panel);
                return;
            }
        };
        let control = control(panel, GROUPS);
        for group in &groups {
            let ticks = group
                .created_ms()
                .saturating_mul(10_000)
                .saturating_add(116444736000000000);
            let date = directory::display_time(FILETIME {
                dwLowDateTime: ticks as u32,
                dwHighDateTime: (ticks >> 32) as u32,
            });
            let text = wide(format!(
                "{} · {} files · {}",
                group.kind.label(),
                grouped(group.items.len() as u64),
                date
            ));
            send(control, CB_ADDSTRING, 0, text.as_ptr() as isize);
        }
        let latest = history
            .preferred
            .take()
            .and_then(|id| groups.iter().position(|group| group.id == id))
            .or_else(|| {
                groups
                    .iter()
                    .position(|group| group.items.iter().any(|item| !item.restored()))
            })
            .or_else(|| (!groups.is_empty()).then_some(0));
        history.model.replace(Model {
            groups,
            ..Model::default()
        });
        if let Some(index) = latest {
            send(control, CB_SETCURSEL, index, 0);
            self.review_history(panel, index);
        } else {
            self.render_history(panel);
        }
    }

    fn review_history(&self, panel: HWND, index: usize) {
        let history = &self.undo_history;
        if history.applying.get() || history.reading.get() {
            return;
        }
        let group = {
            let mut model = history.model.borrow_mut();
            let Some(group) = model.groups.get(index).cloned() else {
                return;
            };
            model.selected = Some(index);
            model.checks.clear();
            model.note = "Checking the current files and original names…".into();
            group
        };
        let id = history.next();
        history.reading.set(true);
        self.render_history(panel);
        let sender = self.sender.clone();
        let owner = self.window.0 as usize;
        if let Err(error) = thread::Builder::new()
            .name("undo-review".into())
            .spawn(move || {
                if sender
                    .send(Event::HistoryReviewed(id, crate::undo::review(&group)))
                    .is_ok()
                {
                    wake(owner);
                }
            })
        {
            history.reading.set(false);
            history.model.borrow_mut().note = error.to_string();
            self.render_history(panel);
        }
    }

    pub fn receive_undo_review(&self, id: usize, checks: Vec<Check>) {
        let history = &self.undo_history;
        history.reading.set(false);
        let panel = history.window.get();
        if panel.0.is_null() {
            return;
        }
        if id != history.generation.get() {
            self.load_history(panel, None);
            return;
        }
        {
            let mut model = history.model.borrow_mut();
            model.checks = checks;
            model.note.clear();
        }
        self.render_history(panel);
    }

    fn undo_ready(&self, panel: HWND) {
        let history = &self.undo_history;
        if history.applying.get() || history.reading.get() {
            return;
        }
        let (group, checks, ready) = {
            let model = history.model.borrow();
            let Some(group) = model.group() else {
                return;
            };
            (group.clone(), model.checks.clone(), model.ready())
        };
        if ready == 0 {
            return;
        }
        if self.operation_active.get() {
            message(
                panel,
                "File operation in progress",
                "Finish the current operation before restoring files.",
                MB_OK | MB_ICONINFORMATION,
            );
            return;
        }
        let generation = history.generation.get();
        let text = format!(
            "Restore {} files to the original paths shown in the review?",
            grouped(ready as u64)
        );
        if message(
            panel,
            "Confirm undo",
            &text,
            MB_OKCANCEL | MB_ICONQUESTION | MB_DEFBUTTON2,
        ) != IDOK
            || self.operation_active.get()
            || history.generation.get() != generation
        {
            return;
        }
        let affected = group
            .items
            .iter()
            .zip(&checks)
            .filter(|(_, check)| **check == Check::Ready)
            .flat_map(|(item, _)| {
                [item.from.parent(), item.to.parent()]
                    .into_iter()
                    .flatten()
                    .map(Path::to_owned)
            })
            .collect();
        history.affected.replace(affected);
        history.applying.set(true);
        self.operation_active.set(true);
        *self.operation_status.borrow_mut() = "Restoring verified files…".into();
        self.status_changed();
        self.render_history(panel);
        let store = self.undo_store.clone();
        let sender = self.sender.clone();
        let owner = self.window.0 as usize;
        if let Err(error) = thread::Builder::new()
            .name("undo-apply".into())
            .spawn(move || {
                if sender
                    .send(Event::UndoFinished(store.apply(group.id, &checks)))
                    .is_ok()
                {
                    wake(owner);
                }
            })
        {
            self.finish_undo(Err(error));
        }
    }

    pub fn undo_running_in(&self, path: &Path) -> bool {
        self.undo_history.applying.get() && self.undo_history.affected.borrow().contains(path)
    }

    pub fn finish_undo(&self, result: std::io::Result<Applied>) {
        let history = &self.undo_history;
        let affected = history.affected.take();
        history.applying.set(false);
        self.operation_active.set(false);
        let touched = match result {
            Ok(result) => {
                let mut model = history.model.borrow_mut();
                model.checks = result.checks;
                model.note = result.save_error.map_or_else(String::new, |error| {
                    format!("Files restored · Could not save history: {error}")
                });
                *self.operation_status.borrow_mut() =
                    format!("Undo · {} restored", grouped(result.touched.len() as u64));
                result.touched
            }
            Err(error) => {
                history.model.borrow_mut().note = format!("Undo could not start · {error}");
                *self.operation_status.borrow_mut() = "Undo could not start".into();
                Vec::new()
            }
        };
        for (index, pane) in self.panes.iter().enumerate() {
            let mut listing = pane.listing.borrow_mut();
            if listing.archive.is_some() || !affected.contains(&listing.path) {
                continue;
            }
            let aliases = touched
                .iter()
                .filter(|(from, to)| {
                    from.parent() == Some(listing.path.as_path())
                        && to.parent() == Some(listing.path.as_path())
                })
                .map(|(from, to)| {
                    (
                        wide(from.file_name().unwrap()),
                        wide(to.file_name().unwrap()),
                    )
                })
                .collect::<HashMap<_, _>>();
            if !aliases.is_empty() {
                listing.pending_batch_names = Some(Arc::new(aliases));
            }
            drop(listing);
            self.refresh(index);
        }
        self.status_changed();
        let panel = history.window.get();
        if !panel.0.is_null() {
            self.render_history(panel);
        }
        self.pump_transfers();
    }

    fn render_history(&self, panel: HWND) {
        let history = &self.undo_history;
        let (count, group_count, ready, status) = {
            let model = history.model.borrow();
            let count = model.group().map_or(0, |group| group.items.len());
            let ready = model.ready();
            let restored = model
                .checks
                .iter()
                .filter(|check| **check == Check::Restored)
                .count();
            let status = if history.applying.get() {
                "Restoring files that still pass the review…".into()
            } else if !model.note.is_empty() {
                model.note.clone()
            } else if history.reading.get() {
                "Waiting for an earlier history read…".into()
            } else if model.groups.is_empty() {
                "No confirmed operations in history.".into()
            } else {
                format!(
                    "{} ready · {} restored · {} blocked",
                    grouped(ready as u64),
                    grouped(restored as u64),
                    grouped((model.checks.len() - ready - restored) as u64)
                )
            };
            (count, model.groups.len(), ready, status)
        };
        let list = control(panel, ITEMS);
        if send(list, LVM_GETITEMCOUNT, 0, 0) != count as isize {
            send(list, LVM_SETITEMCOUNT, count, 0);
        }
        let status = wide(status);
        let busy = history.applying.get() || history.reading.get();
        unsafe {
            let _ = SetWindowTextW(control(panel, SUMMARY), PCWSTR(status.as_ptr()));
            let _ = EnableWindow(control(panel, GROUPS), !busy && group_count > 0);
            let _ = EnableWindow(control(panel, REFRESH), !busy);
            let _ = EnableWindow(control(panel, APPLY), !busy && ready > 0);
            let _ = EnableWindow(control(panel, CLOSE), !history.applying.get());
            let _ = InvalidateRect(Some(list), None, false);
        }
    }

    fn history_text(&self, item: &LVITEMW) {
        let model = self.undo_history.model.borrow();
        let row = item.iItem as usize;
        if let Some(entry) = model.group().and_then(|group| group.items.get(row)) {
            match item.iSubItem {
                0 => copy_text(item, &wide(&entry.to)),
                1 => copy_text(item, &wide(&entry.from)),
                _ => copy_text(item, &wide(model.checks.get(row).map_or("", Check::label))),
            }
        } else {
            copy_text(item, &[0]);
        }
    }

    pub fn update_history_font(&self, font: HFONT) {
        let panel = self.undo_history.window.get();
        if !panel.0.is_null() {
            unsafe {
                let mut child = GetWindow(panel, GW_CHILD);
                while let Ok(window) = child {
                    send(window, WM_SETFONT, font.0 as usize, 1);
                    child = GetWindow(window, GW_HWNDNEXT);
                }
            }
        }
    }
}

fn control(panel: HWND, id: u16) -> HWND {
    unsafe { GetDlgItem(Some(panel), id as i32) }.unwrap_or_default()
}

fn initialize(app: &App, panel: HWND) -> Result<()> {
    unsafe {
        SetWindowTextW(panel, w!("Operation History"))?;
    }
    let normal = WINDOW_EX_STYLE(0);
    let plain = WINDOW_STYLE(0);
    let tab = WS_TABSTOP;
    for control in [
        (
            w!("Static"),
            w!("&Operation"),
            normal,
            plain,
            0,
            [9, 12, 56, 24],
        ),
        (
            w!("ComboBox"),
            w!(""),
            normal,
            tab | WINDOW_STYLE(CBS_DROPDOWNLIST as u32),
            GROUPS,
            [60, 8, 516, 150],
        ),
        (
            WC_LISTVIEWW,
            w!("Undo review"),
            WS_EX_CLIENTEDGE,
            tab | WINDOW_STYLE(LVS_REPORT | LVS_OWNERDATA | LVS_NOSORTHEADER | LVS_SINGLESEL),
            ITEMS,
            [9, 31, 516, 208],
        ),
        (
            w!("Static"),
            w!(""),
            normal,
            plain,
            SUMMARY,
            [9, 213, 516, 229],
        ),
        (
            w!("Button"),
            w!("&Refresh"),
            normal,
            tab,
            REFRESH,
            [9, 234, 85, 252],
        ),
        (
            w!("Button"),
            w!("&Undo ready"),
            normal,
            tab,
            APPLY,
            [92, 234, 188, 252],
        ),
        (
            w!("Button"),
            w!("Close"),
            normal,
            tab,
            CLOSE,
            [440, 234, 516, 252],
        ),
    ] {
        prompt::add_control(panel, app.font.get(), control)?;
    }
    let list = control(panel, ITEMS);
    send(list, CCM_SETUNICODEFORMAT, 1, 0);
    send(
        list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        0,
        (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP) as isize,
    );
    let mut bounds = RECT::default();
    unsafe {
        GetClientRect(list, &mut bounds)?;
    }
    let width = bounds.right - app.scale(19);
    for (index, (title, share)) in [
        (w!("Current path"), 36),
        (w!("Return to"), 36),
        (w!("Result"), 28),
    ]
    .into_iter()
    .enumerate()
    {
        let column = LVCOLUMNW {
            mask: LVCF_WIDTH | LVCF_TEXT,
            cx: width * share / 100,
            pszText: PWSTR(title.as_ptr().cast_mut()),
            ..LVCOLUMNW::default()
        };
        send(
            list,
            LVM_INSERTCOLUMNW,
            index,
            (&column as *const LVCOLUMNW) as isize,
        );
    }
    Ok(())
}

unsafe extern "system" fn panel_proc(
    panel: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    unsafe {
        if msg == WM_INITDIALOG {
            SetWindowLongPtrW(panel, GWLP_USERDATA, lparam.0);
            let app = &*(lparam.0 as *const App);
            if initialize(app, panel).is_err() {
                let _ = DestroyWindow(panel);
                return 0;
            }
            return 1;
        }
        let Some(app) = (GetWindowLongPtrW(panel, GWLP_USERDATA) as *const App).as_ref() else {
            return 0;
        };
        let history = &app.undo_history;
        match msg {
            WM_CLOSE => {
                if !history.applying.get() {
                    let _ = DestroyWindow(panel);
                }
            }
            WM_COMMAND => match wparam.0 as u16 {
                GROUPS if (wparam.0 >> 16) as u16 == CBN_SELCHANGE as u16 => {
                    if let Ok(index) =
                        usize::try_from(send(control(panel, GROUPS), CB_GETCURSEL, 0, 0))
                    {
                        app.review_history(panel, index);
                    }
                }
                REFRESH => {
                    let selected = history.model.borrow().group().map(|group| group.id);
                    app.load_history(panel, selected);
                }
                APPLY => app.undo_ready(panel),
                CLOSE => {
                    if !history.applying.get() {
                        let _ = DestroyWindow(panel);
                    }
                }
                _ => return 0,
            },
            WM_NOTIFY => {
                let header = &*(lparam.0 as *const NMHDR);
                if header.idFrom == ITEMS as usize && header.code == LVN_GETDISPINFOW {
                    let request = &*(lparam.0 as *const NMLVDISPINFOW);
                    if request.item.mask.contains(LVIF_TEXT) {
                        app.history_text(&request.item);
                    }
                }
                return 0;
            }
            WM_NCDESTROY => {
                history.window.set(HWND::default());
                history.next();
                history.model.replace(Model::default());
                SetWindowLongPtrW(panel, GWLP_USERDATA, 0);
                return 0;
            }
            _ => return 0,
        }
    }
    1
}
