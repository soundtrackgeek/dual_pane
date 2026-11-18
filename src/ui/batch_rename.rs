use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::batch_rename::{self, Outcome, Plan, Report, Rules, Selection};

const FOLDER: u16 = 661;
const FIND: u16 = 662;
const REPLACE: u16 = 663;
const PREFIX: u16 = 664;
const SUFFIX: u16 = 665;
const KEEP: u16 = 666;
const NUMBER: u16 = 667;
const FIRST: u16 = 668;
const DIGITS: u16 = 669;
const ROWS: u16 = 670;
const SUMMARY: u16 = 671;
const REVIEW: u16 = IDOK.0 as u16;
const CLOSE: u16 = IDCANCEL.0 as u16;
const APPLY: u16 = 672;

#[derive(Default)]
pub(super) struct BatchRename {
    pub window: Cell<HWND>,
    selection: RefCell<Option<Arc<Selection>>>,
    plan: RefCell<Option<Arc<Plan>>>,
    report: RefCell<Option<Report>>,
    review: RefCell<Option<Arc<AtomicBool>>>,
    generation: Cell<usize>,
    worker: Cell<bool>,
    applying: Cell<bool>,
    note: RefCell<String>,
}

impl BatchRename {
    fn invalidate(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        if let Some(review) = self.review.take() {
            review.store(false, Ordering::Relaxed);
        }
        self.plan.take();
        self.report.take();
        self.note.borrow_mut().clear();
    }
}

impl App {
    pub fn show_batch_rename(&self) {
        let batch = &self.batch_rename;
        let existing = batch.window.get();
        if !existing.0.is_null() {
            unsafe {
                let _ = SetForegroundWindow(existing);
            }
            return;
        }
        let pane = &self.panes[self.active.get()];
        let listing = pane.listing.borrow();
        if !listing.ready() || pane.count() == 0 {
            drop(listing);
            message(
                self.window,
                "Batch Rename",
                "Select files in the active pane first.",
                MB_OK | MB_ICONINFORMATION,
            );
            return;
        }
        let selection = Arc::new(Selection {
            folder: listing.path.clone(),
            entries: pane
                .selection()
                .filter_map(|row| listing.rows.get(row).cloned())
                .collect(),
        });
        drop(listing);
        match prompt::create_panel(
            self.window,
            [500, 293],
            WINDOW_STYLE(0),
            Some(panel_proc),
            LPARAM((self as *const App) as isize),
        ) {
            Ok(panel) if unsafe { IsWindow(Some(panel)).as_bool() } => {
                batch.window.set(panel);
                show_path(control(panel, FOLDER), &selection.folder);
                let count = selection.entries.len();
                batch.selection.replace(Some(selection));
                send(control(panel, ROWS), LVM_SETITEMCOUNT, count, 0);
                self.render_batch(panel);
                unsafe {
                    let _ = ShowWindow(panel, SW_SHOW);
                    let _ = SetFocus(Some(control(panel, FIND)));
                }
            }
            Ok(_) => {}
            Err(error) => {
                message(
                    self.window,
                    "Could not open Batch Rename",
                    &error.message(),
                    MB_OK | MB_ICONWARNING,
                );
            }
        }
    }

    fn review_batch(&self, panel: HWND) {
        let batch = &self.batch_rename;
        if batch.worker.get()
            || batch.applying.get()
            || batch
                .report
                .borrow()
                .as_ref()
                .is_some_and(|report| report.renamed > 0)
        {
            return;
        }
        let Some(selection) = batch.selection.borrow().clone() else {
            return;
        };
        let Ok(rules) = read_rules(panel) else {
            set_text(
                control(panel, SUMMARY),
                "Use a start number from 0 to 4,294,967,295 and 1 to 12 digits.",
            );
            return;
        };
        batch.invalidate();
        let id = batch.generation.get();
        let current = Arc::new(AtomicBool::new(true));
        batch.review.replace(Some(current.clone()));
        batch.worker.set(true);
        let owner = self.window.0 as usize;
        let sender = self.sender.clone();
        if let Err(error) = thread::Builder::new()
            .name("rename-review".into())
            .spawn(move || {
                let plan =
                    batch_rename::review(selection, rules, || current.load(Ordering::Relaxed));
                if sender.send(Event::RenameReviewed(id, plan)).is_ok() {
                    wake(owner);
                }
            })
        {
            self.receive_batch_review(id, Err(error));
        }
        self.render_batch(panel);
    }

    pub fn receive_batch_review(&self, id: usize, result: std::io::Result<Plan>) {
        let batch = &self.batch_rename;
        batch.worker.set(false);
        if id == batch.generation.get() {
            batch.review.take();
            match result {
                Ok(plan) => {
                    batch.plan.replace(Some(Arc::new(plan)));
                }
                Err(error) => {
                    *batch.note.borrow_mut() = format!("Could not review the selection · {error}")
                }
            }
        }
        let panel = batch.window.get();
        if !panel.0.is_null() {
            self.render_batch(panel);
        }
    }

    fn apply_batch(&self, panel: HWND) {
        let batch = &self.batch_rename;
        if batch.applying.get() || batch.report.borrow().is_some() {
            return;
        }
        let plan = batch
            .plan
            .borrow()
            .clone()
            .filter(|plan| plan.ready > 0 && plan.conflicts == 0);
        let Some(plan) = plan else {
            return;
        };
        if self.operation_active.get() {
            message(
                panel,
                "File operation in progress",
                "Wait for the current file operation to finish.",
                MB_OK | MB_ICONINFORMATION,
            );
            return;
        }
        let id = batch.generation.get();
        let confirmation = format!(
            "Rename {} files in this folder?\n\n{}",
            grouped(plan.ready as u64),
            plan.selection.folder.display()
        );
        if message(
            panel,
            "Apply batch rename",
            &confirmation,
            MB_OKCANCEL | MB_ICONQUESTION | MB_DEFBUTTON2,
        ) != IDOK
            || id != batch.generation.get()
            || self.operation_active.get()
        {
            return;
        }
        batch.applying.set(true);
        self.operation_active.set(true);
        *self.operation_status.borrow_mut() = "Batch rename in progress…".into();
        self.status_changed();
        self.render_batch(panel);
        let job = plan.clone();
        let sender = self.sender.clone();
        let owner = self.window.0 as usize;
        if let Err(error) = thread::Builder::new()
            .name("batch-rename".into())
            .spawn(move || {
                let report = batch_rename::apply(&job);
                if sender.send(Event::RenameFinished(report)).is_ok() {
                    wake(owner);
                }
            })
        {
            self.finish_batch_rename(Report::stopped(&plan, error.to_string()));
        }
    }

    pub fn batch_running_in(&self, path: &Path) -> bool {
        self.batch_rename.applying.get()
            && self
                .batch_rename
                .selection
                .borrow()
                .as_ref()
                .is_some_and(|selection| selection.folder == path)
    }

    pub fn finish_batch_rename(&self, report: Report) {
        let batch = &self.batch_rename;
        batch.applying.set(false);
        self.operation_active.set(false);
        *self.operation_status.borrow_mut() =
            format!("Batch rename · {} renamed", grouped(report.renamed as u64));
        if let Some(plan) = batch.plan.borrow().clone() {
            let renamed = (report.renamed > 0).then(|| {
                Arc::new(
                    plan.selection
                        .entries
                        .iter()
                        .zip(&plan.rows)
                        .zip(&report.rows)
                        .filter(|(_, outcome)| matches!(outcome, Outcome::Renamed))
                        .map(|((original, row), _)| (original.name.to_vec(), row.target.to_vec()))
                        .collect(),
                )
            });
            for (index, pane) in self.panes.iter().enumerate() {
                let mut listing = pane.listing.borrow_mut();
                if listing.path != plan.selection.folder {
                    continue;
                }
                listing.pending_batch_names = renamed.clone();
                drop(listing);
                self.refresh(index);
            }
        }
        batch.report.replace(Some(report));
        self.status_changed();
        self.render_batch(batch.window.get());
        self.pump_transfers();
    }

    fn render_batch(&self, panel: HWND) {
        let batch = &self.batch_rename;
        let applying = batch.applying.get();
        let count = batch
            .selection
            .borrow()
            .as_ref()
            .map_or(0, |selection| selection.entries.len());
        let (summary, ready, finished) = {
            let report = batch.report.borrow();
            let plan = batch.plan.borrow();
            let note = batch.note.borrow();
            if applying {
                (
                    "Checking the reviewed files and applying names…".into(),
                    false,
                    false,
                )
            } else if let Some(report) = report.as_ref() {
                let summary = format!(
                    "{} renamed · {}",
                    grouped(report.renamed as u64),
                    report
                        .reason
                        .as_deref()
                        .unwrap_or("Finished. Reopen for a new batch.")
                );
                (summary, false, report.renamed > 0)
            } else if batch.review.borrow().is_some() {
                ("Reviewing names and file identities…".into(), false, false)
            } else if !note.is_empty() {
                (note.clone(), false, false)
            } else if let Some(plan) = plan.as_ref() {
                (
                    format!(
                        "{} ready · {} unchanged · {} conflicts",
                        grouped(plan.ready as u64),
                        grouped((count - plan.ready - plan.conflicts) as u64),
                        grouped(plan.conflicts as u64)
                    ),
                    plan.ready > 0 && plan.conflicts == 0,
                    false,
                )
            } else if batch.worker.get() {
                (
                    "Waiting for the earlier review to return…".into(),
                    false,
                    false,
                )
            } else {
                (
                    format!(
                        "{} selected · Review to see the proposed names",
                        grouped(count as u64)
                    ),
                    false,
                    false,
                )
            }
        };
        set_text(control(panel, SUMMARY), &summary);
        let editable = !applying && !finished;
        for id in [FIND, REPLACE, PREFIX, SUFFIX, KEEP, NUMBER] {
            unsafe {
                let _ = EnableWindow(control(panel, id), editable);
            }
        }
        let numbering = editable && checked(panel, NUMBER);
        unsafe {
            let _ = EnableWindow(control(panel, FIRST), numbering);
            let _ = EnableWindow(control(panel, DIGITS), numbering);
            let _ = EnableWindow(control(panel, REVIEW), editable && !batch.worker.get());
            let _ = EnableWindow(control(panel, APPLY), ready);
            let _ = EnableWindow(control(panel, CLOSE), !applying);
            let _ = InvalidateRect(Some(control(panel, ROWS)), None, false);
        }
    }

    fn batch_row_text(&self, item: &LVITEMW) {
        let batch = &self.batch_rename;
        let selection = batch.selection.borrow();
        let plan = batch.plan.borrow();
        let report = batch.report.borrow();
        let index = item.iItem as usize;
        let row = plan.as_ref().and_then(|plan| plan.rows.get(index));
        match item.iSubItem {
            0 => {
                let original = selection
                    .as_ref()
                    .and_then(|selection| selection.entries.get(index));
                copy_text(item, original.map_or(&[0], |entry| entry.name.as_ref()));
            }
            1 => copy_text(item, row.map_or(&[0], |row| row.target.as_ref())),
            _ => {
                let status = report
                    .as_ref()
                    .and_then(|report| report.rows.get(index))
                    .map(Outcome::label)
                    .or_else(|| row.map(|row| row.check.label()))
                    .unwrap_or("");
                copy_text(item, &wide(status));
            }
        }
    }

    pub fn update_batch_font(&self, font: HFONT) {
        let panel = self.batch_rename.window.get();
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

fn set_text(window: HWND, text: &str) {
    let text = wide(text);
    unsafe {
        let _ = SetWindowTextW(window, PCWSTR(text.as_ptr()));
    }
}

fn checked(panel: HWND, id: u16) -> bool {
    send(control(panel, id), BM_GETCHECK, 0, 0) == BST_CHECKED.0 as isize
}

fn read_rules(panel: HWND) -> std::result::Result<Rules, ()> {
    let text = |id| window_text(control(panel, id));
    let numbering = if checked(panel, NUMBER) {
        let first: u32 = text(FIRST).to_string_lossy().parse().map_err(|_| ())?;
        let digits: usize = text(DIGITS).to_string_lossy().parse().map_err(|_| ())?;
        if !(1..=12).contains(&digits) {
            return Err(());
        }
        Some((first, digits))
    } else {
        None
    };
    Ok(Rules {
        find: text(FIND).encode_wide().collect(),
        replace: text(REPLACE).encode_wide().collect(),
        prefix: text(PREFIX).encode_wide().collect(),
        suffix: text(SUFFIX).encode_wide().collect(),
        numbering,
        keep_extension: checked(panel, KEEP),
    })
}

fn initialize(app: &App, panel: HWND) -> Result<()> {
    unsafe {
        SetWindowTextW(panel, w!("Batch Rename"))?;
    }
    let normal = WINDOW_EX_STYLE(0);
    let edge = WS_EX_CLIENTEDGE;
    let label = WINDOW_STYLE(0);
    let tab = WS_TABSTOP;
    let edit = tab | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
    let numeric = edit | WINDOW_STYLE(ES_NUMBER as u32);
    let check = tab | WINDOW_STYLE(BS_AUTOCHECKBOX as u32);
    for template in [
        (
            w!("Static"),
            w!("Folder"),
            normal,
            label,
            0,
            [9, 10, 44, 22],
        ),
        (
            w!("Edit"),
            w!(""),
            edge,
            edit | WINDOW_STYLE(ES_READONLY as u32),
            FOLDER,
            [50, 7, 491, 22],
        ),
        (w!("Static"), w!("&Find"), normal, label, 0, [9, 34, 44, 46]),
        (w!("Edit"), w!(""), edge, edit, FIND, [50, 30, 241, 46]),
        (
            w!("Static"),
            w!("Re&place"),
            normal,
            label,
            0,
            [250, 34, 295, 46],
        ),
        (w!("Edit"), w!(""), edge, edit, REPLACE, [300, 30, 491, 46]),
        (
            w!("Static"),
            w!("Prefi&x"),
            normal,
            label,
            0,
            [9, 55, 44, 68],
        ),
        (w!("Edit"), w!(""), edge, edit, PREFIX, [50, 51, 241, 68]),
        (
            w!("Static"),
            w!("S&uffix"),
            normal,
            label,
            0,
            [250, 55, 295, 68],
        ),
        (w!("Edit"), w!(""), edge, edit, SUFFIX, [300, 51, 491, 68]),
        (
            w!("Button"),
            w!("&Keep extensions"),
            normal,
            check,
            KEEP,
            [9, 76, 140, 92],
        ),
        (
            w!("Button"),
            w!("Append &number"),
            normal,
            check,
            NUMBER,
            [154, 76, 256, 92],
        ),
        (
            w!("Static"),
            w!("Start at"),
            normal,
            label,
            0,
            [268, 79, 304, 92],
        ),
        (
            w!("Edit"),
            w!("1"),
            edge,
            numeric,
            FIRST,
            [309, 75, 371, 92],
        ),
        (
            w!("Static"),
            w!("Digits"),
            normal,
            label,
            0,
            [385, 79, 420, 92],
        ),
        (
            w!("Edit"),
            w!("3"),
            edge,
            numeric,
            DIGITS,
            [426, 75, 491, 92],
        ),
        (
            WC_LISTVIEWW,
            w!("Rename preview"),
            edge,
            tab | WINDOW_STYLE(LVS_REPORT | LVS_OWNERDATA | LVS_NOSORTHEADER | LVS_SINGLESEL),
            ROWS,
            [9, 101, 491, 244],
        ),
        (
            w!("Static"),
            w!(""),
            normal,
            label,
            SUMMARY,
            [9, 251, 491, 265],
        ),
        (
            w!("Button"),
            w!("&Review"),
            normal,
            tab | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            REVIEW,
            [9, 269, 86, 286],
        ),
        (
            w!("Button"),
            w!("&Apply"),
            normal,
            tab,
            APPLY,
            [92, 269, 169, 286],
        ),
        (
            w!("Button"),
            w!("Close"),
            normal,
            tab,
            CLOSE,
            [414, 269, 491, 286],
        ),
    ] {
        prompt::add_control(panel, app.font.get(), template)?;
    }
    for id in [FIND, REPLACE, PREFIX, SUFFIX] {
        send(control(panel, id), EM_SETLIMITTEXT, 255, 0);
    }
    send(control(panel, FIRST), EM_SETLIMITTEXT, 10, 0);
    send(control(panel, DIGITS), EM_SETLIMITTEXT, 2, 0);
    send(control(panel, KEEP), BM_SETCHECK, BST_CHECKED.0 as usize, 0);
    send(panel, DM_SETDEFID, REVIEW.into(), 0);
    let list = control(panel, ROWS);
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
    for (index, (title, percent)) in [
        (w!("Original name"), 33),
        (w!("Proposed name"), 33),
        (w!("Status"), 34),
    ]
    .into_iter()
    .enumerate()
    {
        let column = LVCOLUMNW {
            mask: LVCF_WIDTH | LVCF_TEXT,
            cx: width * percent / 100,
            pszText: PWSTR(title.as_ptr().cast_mut()),
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
        let batch = &app.batch_rename;
        match msg {
            WM_CLOSE => {
                if !batch.applying.get() {
                    let _ = DestroyWindow(panel);
                }
            }
            WM_NCDESTROY => {
                batch.window.set(HWND::default());
                batch.invalidate();
                batch.selection.take();
                SetWindowLongPtrW(panel, GWLP_USERDATA, 0);
                return 0;
            }
            WM_COMMAND if batch.window.get() == panel => {
                let id = wparam.0 as u16;
                let code = (wparam.0 >> 16) as u16;
                match id {
                    REVIEW => app.review_batch(panel),
                    APPLY => app.apply_batch(panel),
                    CLOSE => {
                        if !batch.applying.get() {
                            let _ = DestroyWindow(panel);
                        }
                    }
                    id if (((FIND..=SUFFIX).contains(&id) || matches!(id, FIRST | DIGITS))
                        && code == EN_CHANGE as u16)
                        || (matches!(id, KEEP | NUMBER) && code == BN_CLICKED as u16) =>
                    {
                        if !batch.applying.get()
                            && batch
                                .report
                                .borrow()
                                .as_ref()
                                .is_none_or(|report| report.renamed == 0)
                        {
                            batch.invalidate();
                            app.render_batch(panel);
                        }
                    }
                    _ => return 0,
                }
            }
            WM_NOTIFY => {
                let header = &*(lparam.0 as *const NMHDR);
                if header.idFrom == ROWS as usize && header.code == LVN_GETDISPINFOW {
                    let data = &*(lparam.0 as *const NMLVDISPINFOW);
                    if data.item.mask.contains(LVIF_TEXT) {
                        app.batch_row_text(&data.item);
                    }
                }
                return 0;
            }
            _ => return 0,
        }
    }
    1
}
