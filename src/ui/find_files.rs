use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc::{SyncSender, sync_channel},
};

use super::*;
use crate::find::{self, Hit, Query, Report, Update};

const QUERY: u16 = 621;
const SCOPE: u16 = 622;
const RESULTS: u16 = 623;
const SUMMARY: u16 = 624;
const WARNING: u16 = 625;
const STOP: u16 = 626;
const OPEN: u16 = 627;
const SEARCH: u16 = IDOK.0 as u16;
const CLOSE: u16 = IDCANCEL.0 as u16;
const MAX_RESULTS: usize = 100_000;
const RESULT_BUDGET: usize = 32 * 1024 * 1024;
const WORKERS_PER_PANE: usize = 2;

struct WorkerSlot(Arc<AtomicUsize>);

impl WorkerSlot {
    fn acquire(count: &Arc<AtomicUsize>) -> Option<Self> {
        count
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                (value < WORKERS_PER_PANE).then_some(value + 1)
            })
            .ok()?;
        Some(Self(count.clone()))
    }
}

impl Drop for WorkerSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub(super) struct FindFiles {
    pub window: Cell<HWND>,
    generation: Cell<usize>,
    session: RefCell<Option<Session>>,
    workers: [Arc<AtomicUsize>; 2],
}

struct Session {
    stopped: Arc<AtomicBool>,
    receiver: Option<Receiver<Update>>,
    rows: Vec<Hit>,
    last_folder: [Option<Arc<PathBuf>>; 2],
    bytes: usize,
    pending: usize,
    report: Report,
    status: &'static str,
}

impl Session {
    fn stop(&mut self, status: &'static str) {
        self.stopped.store(true, Ordering::Relaxed);
        self.receiver = None;
        self.status = status;
    }

    fn append(&mut self, hits: Vec<Hit>) {
        for hit in hits {
            let last = &mut self.last_folder[hit.pane];
            let mut bytes = 2 * size_of::<Hit>() + 2 * hit.name.len();
            if last
                .as_ref()
                .is_none_or(|folder| !Arc::ptr_eq(folder, &hit.folder))
            {
                bytes += size_of::<PathBuf>() + hit.folder.as_os_str().len();
                *last = Some(hit.folder.clone());
            }
            if self.rows.len() == MAX_RESULTS || self.bytes + bytes > RESULT_BUDGET {
                self.stop("Result limit reached · narrow the search for more");
                break;
            }
            self.bytes += bytes;
            self.rows.push(hit);
        }
    }

    fn finish(&mut self, report: Report) {
        self.report.unreadable += report.unreadable;
        self.report.links += report.links;
        if self.report.first_error.is_none() {
            self.report.first_error = report.first_error;
        }
        self.pending -= 1;
        if self.pending == 0 {
            self.stop(if self.report.unreadable == 0 && self.report.links == 0 {
                "Search complete"
            } else {
                "Search complete with skipped folders"
            });
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
    }
}

impl App {
    pub fn update_find_font(&self, font: HFONT) {
        let panel = self.find_files.window.get();
        if panel.0.is_null() {
            return;
        }
        unsafe {
            let mut child = GetWindow(panel, GW_CHILD);
            while let Ok(window) = child {
                send(window, WM_SETFONT, font.0 as usize, 1);
                child = GetWindow(window, GW_HWNDNEXT);
            }
        }
    }

    pub fn show_find_files(&self) {
        let existing = self.find_files.window.get();
        if !existing.0.is_null() {
            unsafe {
                let _ = ShowWindow(existing, SW_RESTORE);
                let _ = SetForegroundWindow(existing);
                let _ = SetFocus(Some(control(existing, QUERY)));
            }
            send(control(existing, QUERY), EM_SETSEL, 0, -1);
            return;
        }
        match prompt::create_panel(
            self.window,
            [475, 251],
            Some(panel_proc),
            LPARAM((self as *const App) as isize),
        ) {
            Ok(window) if unsafe { IsWindow(Some(window)).as_bool() } => {
                self.find_files.window.set(window);
                self.render_find(window);
                unsafe {
                    let _ = ShowWindow(window, SW_SHOW);
                    let _ = SetFocus(Some(control(window, QUERY)));
                }
            }
            Ok(_) => {}
            Err(error) => {
                message(
                    self.window,
                    "Could not open Find Files",
                    &error.message(),
                    MB_OK | MB_ICONWARNING,
                );
            }
        }
    }

    fn start_find(&self, window: HWND) {
        let query = window_text(control(window, QUERY));
        if query.is_empty() {
            message(
                window,
                "Find Files",
                "Enter part of a filename or a wildcard such as *.flac.",
                MB_OK | MB_ICONINFORMATION,
            );
            unsafe {
                let _ = SetFocus(Some(control(window, QUERY)));
            }
            return;
        }
        let query = Arc::new(Query::new(query.encode_wide().collect()));
        let active = self.active.get();
        let both = send(control(window, SCOPE), CB_GETCURSEL, 0, 0) == 1;
        let generation = self.find_files.generation.get().wrapping_add(1);
        self.find_files.generation.set(generation);

        let (sender, receiver) = sync_channel(8);
        let stopped = Arc::new(AtomicBool::new(false));
        self.find_files.session.replace(Some(Session {
            stopped: stopped.clone(),
            receiver: Some(receiver),
            rows: Vec::new(),
            last_folder: Default::default(),
            bytes: 0,
            pending: 1 + usize::from(both),
            report: Report::default(),
            status: "Searching…",
        }));
        send(control(window, RESULTS), LVM_SETITEMCOUNT, 0, 0);

        for pane in [active, 1 - active].into_iter().take(1 + usize::from(both)) {
            let path = self.panes[pane].listing.borrow().path.clone();
            let worker = Worker {
                pane,
                path: path.clone(),
                query: query.clone(),
                stopped: stopped.clone(),
                sender: sender.clone(),
                owner: self.window.0 as usize,
                generation,
            };
            let result = WorkerSlot::acquire(&self.find_files.workers[pane])
                .ok_or_else(|| {
                    "Earlier searches are still waiting on Windows. Try again when they return."
                        .to_owned()
                })
                .and_then(|slot| worker.start(slot).map_err(|error| error.to_string()));
            if let Err(error) = result {
                self.find_files
                    .session
                    .borrow_mut()
                    .as_mut()
                    .unwrap()
                    .finish(Report {
                        unreadable: 1,
                        first_error: Some(format!("{} · {error}", path.display())),
                        ..Report::default()
                    });
            }
        }
        self.render_find(window);
    }

    pub fn receive_find_update(&self, generation: usize) {
        if generation != self.find_files.generation.get() {
            return;
        }
        let window = self.find_files.window.get();
        let mut model = self.find_files.session.borrow_mut();
        let Some(session) = model.as_mut() else {
            return;
        };
        let Some(update) = session
            .receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
        else {
            return;
        };
        match update {
            Update::Hits(hits) => session.append(hits),
            Update::Finished(report) => session.finish(report),
        }
        // Release the model before the owner-data list can ask for row text.
        drop(model);
        self.render_find(window);
    }

    fn stop_find(&self, window: HWND) {
        if let Some(session) = self.find_files.session.borrow_mut().as_mut()
            && session.receiver.is_some()
        {
            session.stop("Search stopped");
        }
        self.render_find(window);
    }

    fn render_find(&self, window: HWND) {
        let model = self.find_files.session.borrow();
        let (count, searching, status, warning) = match model.as_ref() {
            None => (
                0,
                false,
                "Search subfolders by filename. Use * or ? for wildcards.".into(),
                None,
            ),
            Some(session) => {
                let count = session.rows.len();
                let report = &session.report;
                let mut status = format!(
                    "{} · {} {}",
                    session.status,
                    grouped(count as u64),
                    if count == 1 { "match" } else { "matches" }
                );
                if report.unreadable > 0 {
                    status.push_str(&format!(
                        " · {} unreadable",
                        grouped(report.unreadable as u64)
                    ));
                }
                if report.links > 0 {
                    status.push_str(&format!(
                        " · {} linked {} skipped",
                        grouped(report.links as u64),
                        if report.links == 1 {
                            "folder"
                        } else {
                            "folders"
                        }
                    ));
                }
                (
                    count,
                    session.receiver.is_some(),
                    status,
                    report.first_error.clone(),
                )
            }
        };
        drop(model);
        let list = control(window, RESULTS);
        let previous = send(list, LVM_GETITEMCOUNT, 0, 0);
        if previous != count as isize {
            send(
                list,
                LVM_SETITEMCOUNT,
                count,
                (LVSICF_NOINVALIDATEALL | LVSICF_NOSCROLL) as isize,
            );
            if previous == 0 && count > 0 {
                let state = LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0);
                item_state(list, 0, state, state);
            }
        }
        set_text(control(window, SUMMARY), &status);
        let detail = warning.unwrap_or_default();
        let field = control(window, WARNING);
        if window_text(field) != OsString::from(&detail) {
            set_text(field, &detail);
        }
        unsafe {
            let _ = EnableWindow(control(window, STOP), searching);
            let _ = ShowWindow(
                field,
                if detail.is_empty() {
                    SW_HIDE
                } else {
                    SW_SHOWNA
                },
            );
        }
        enable_open(window);
    }

    fn find_text(&self, item: &LVITEMW) {
        let session = self.find_files.session.borrow();
        if let Some(hit) = session
            .as_ref()
            .and_then(|session| session.rows.get(item.iItem as usize))
        {
            match item.iSubItem {
                0 => copy_text(item, &hit.name),
                1 => copy_text(item, &wide(hit.folder.as_os_str())),
                _ => copy_text(item, &wide(if hit.pane == 0 { "Left" } else { "Right" })),
            }
        } else {
            copy_text(item, &[0]);
        }
    }

    fn open_find_hit(&self, window: HWND, row: Option<usize>) {
        let list = control(window, RESULTS);
        let row = row.or_else(|| {
            usize::try_from(send(
                list,
                LVM_GETNEXTITEM,
                usize::MAX,
                LVNI_SELECTED as isize,
            ))
            .ok()
        });
        let target = self
            .find_files
            .session
            .borrow()
            .as_ref()
            .and_then(|session| {
                let hit = session.rows.get(row?)?;
                let name = &hit.name[..hit.name.len() - 1];
                let mut location = Location::new((*hit.folder).clone());
                location.anchors.focused = name.to_vec();
                location.anchors.selected = name.to_vec();
                location.anchors.top = name.to_vec();
                Some((hit.pane, location))
            });
        let Some((pane, location)) = target else {
            return;
        };
        self.capture_tab(pane);
        if !self.panes[pane]
            .navigation
            .borrow_mut()
            .open_location(location)
        {
            message(
                window,
                "Tab limit reached",
                "Close a tab on the result’s side, then open the result again.",
                MB_OK | MB_ICONINFORMATION,
            );
            return;
        }
        self.rebuild_tabs(pane);
        self.activate_tab(pane);
        unsafe {
            let _ = ShowWindow(window, SW_HIDE);
            let _ = SetForegroundWindow(self.window);
        }
        self.focus_list(pane);
    }

    pub fn find_panel_key(&self, panel: HWND, msg: &MSG) -> bool {
        if panel != self.find_files.window.get() || msg.message != WM_KEYDOWN {
            return false;
        }
        if VIRTUAL_KEY(msg.wParam.0 as u16) == VK_RETURN && msg.hwnd == control(panel, RESULTS) {
            self.open_find_hit(panel, None);
            return true;
        }
        false
    }
}

struct Worker {
    pane: usize,
    path: PathBuf,
    query: Arc<Query>,
    stopped: Arc<AtomicBool>,
    sender: SyncSender<Update>,
    owner: usize,
    generation: usize,
}

impl Worker {
    fn start(self, slot: WorkerSlot) -> std::io::Result<()> {
        thread::Builder::new()
            .name("find-files".into())
            .spawn(move || {
                let _slot = slot;
                let current = || !self.stopped.load(Ordering::Relaxed);
                find::scan(self.path, self.pane, &self.query, current, |update| {
                    current()
                        && self.sender.send(update).is_ok()
                        && unsafe {
                            PostMessageW(
                                Some(HWND(self.owner as _)),
                                FIND_WAKE,
                                WPARAM(self.generation),
                                LPARAM(0),
                            )
                            .is_ok()
                        }
                });
            })?;
        Ok(())
    }
}

fn control(window: HWND, id: u16) -> HWND {
    unsafe { GetDlgItem(Some(window), id as i32) }.unwrap_or_default()
}

fn enable_open(window: HWND) {
    let selected = send(
        control(window, RESULTS),
        LVM_GETNEXTITEM,
        usize::MAX,
        LVNI_SELECTED as isize,
    ) >= 0;
    unsafe {
        let _ = EnableWindow(control(window, OPEN), selected);
    }
}

fn set_text(window: HWND, text: &str) {
    let text = wide(text);
    unsafe {
        let _ = SetWindowTextW(window, PCWSTR(text.as_ptr()));
    }
}

fn initialize(app: &App, window: HWND) -> Result<()> {
    unsafe {
        SetWindowTextW(window, w!("Find Files"))?;
    }
    let normal = WINDOW_EX_STYLE::default();
    let static_style = WINDOW_STYLE(0);
    let tab = WS_TABSTOP;
    let edit = WINDOW_STYLE(ES_AUTOHSCROLL as u32);
    let controls = [
        (
            w!("Static"),
            w!("File&name"),
            normal,
            static_style,
            0,
            [9, 12, 56, 24],
        ),
        (
            w!("Edit"),
            w!(""),
            WS_EX_CLIENTEDGE,
            tab | edit,
            QUERY,
            [59, 8, 287, 24],
        ),
        (
            w!("Static"),
            w!("&In"),
            normal,
            static_style,
            0,
            [295, 12, 309, 24],
        ),
        (
            w!("ComboBox"),
            w!(""),
            normal,
            tab | WINDOW_STYLE(CBS_DROPDOWNLIST as u32),
            SCOPE,
            [313, 8, 466, 65],
        ),
        (
            w!("Button"),
            w!("&Find"),
            normal,
            tab | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            SEARCH,
            [59, 30, 116, 47],
        ),
        (
            w!("Button"),
            w!("&Stop"),
            normal,
            tab,
            STOP,
            [122, 30, 179, 47],
        ),
        (
            WC_LISTVIEWW,
            w!("Search results"),
            WS_EX_CLIENTEDGE,
            tab | WINDOW_STYLE(
                LVS_REPORT | LVS_OWNERDATA | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_NOSORTHEADER,
            ),
            RESULTS,
            [9, 55, 466, 193],
        ),
        (
            w!("Static"),
            w!(""),
            normal,
            static_style,
            SUMMARY,
            [9, 199, 466, 212],
        ),
        (
            w!("Edit"),
            w!(""),
            normal,
            tab | edit | WINDOW_STYLE(ES_READONLY as u32),
            WARNING,
            [9, 213, 466, 227],
        ),
        (
            w!("Button"),
            w!("&Open folder in new tab"),
            normal,
            tab,
            OPEN,
            [269, 228, 402, 245],
        ),
        (
            w!("Button"),
            w!("Close"),
            normal,
            tab,
            CLOSE,
            [409, 228, 466, 245],
        ),
    ];
    for template in controls {
        prompt::add_control(window, app.font.get(), template)?;
    }
    send(control(window, QUERY), EM_SETLIMITTEXT, 255, 0);
    let scope = control(window, SCOPE);
    for label in [
        w!("Active folder and subfolders"),
        w!("Both folders and subfolders"),
    ] {
        send(scope, CB_ADDSTRING, 0, label.as_ptr() as isize);
    }
    send(scope, CB_SETCURSEL, 0, 0);
    send(window, DM_SETDEFID, SEARCH.into(), 0);
    let list = control(window, RESULTS);
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
    for (index, (name, percent)) in [
        (w!("Filename"), 32),
        (w!("Containing folder"), 58),
        (w!("Pane"), 10),
    ]
    .into_iter()
    .enumerate()
    {
        let column = LVCOLUMNW {
            mask: LVCF_WIDTH | LVCF_TEXT,
            cx: width * percent / 100,
            pszText: PWSTR(name.as_ptr().cast_mut()),
            ..Default::default()
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
    window: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    unsafe {
        if msg == WM_INITDIALOG {
            SetWindowLongPtrW(window, GWLP_USERDATA, lparam.0);
            let app = &*(lparam.0 as *const App);
            if initialize(app, window).is_err() {
                let _ = DestroyWindow(window);
                return 0;
            }
            return 1;
        }
        let Some(app) = (GetWindowLongPtrW(window, GWLP_USERDATA) as *const App).as_ref() else {
            return 0;
        };
        match msg {
            WM_CLOSE => {
                let _ = DestroyWindow(window);
            }
            WM_NCDESTROY => {
                app.find_files.window.set(HWND::default());
                app.find_files.session.replace(None);
                SetWindowLongPtrW(window, GWLP_USERDATA, 0);
                return 0;
            }
            WM_COMMAND => match wparam.0 as u16 {
                SEARCH => app.start_find(window),
                STOP => app.stop_find(window),
                OPEN => app.open_find_hit(window, None),
                CLOSE => {
                    let _ = DestroyWindow(window);
                }
                _ => return 0,
            },
            WM_NOTIFY => {
                let header = &*(lparam.0 as *const NMHDR);
                if header.idFrom != RESULTS as usize {
                    return 0;
                }
                match header.code {
                    LVN_ITEMCHANGED | LVN_ODSTATECHANGED => enable_open(window),
                    LVN_GETDISPINFOW => {
                        let data = &*(lparam.0 as *const NMLVDISPINFOW);
                        if data.item.mask.contains(LVIF_TEXT) {
                            app.find_text(&data.item);
                        }
                    }
                    NM_DBLCLK => {
                        let data = &*(lparam.0 as *const NMITEMACTIVATE);
                        if let Ok(row) = usize::try_from(data.iItem) {
                            app.open_find_hit(window, Some(row));
                        }
                    }
                    _ => {}
                }
                return 0;
            }
            _ => return 0,
        }
    }
    1
}
