use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::transfer::{self, ItemState, Progress, ProgressUpdate, Report, Request};

const LIMIT: usize = 32;
const HISTORY_JOBS: usize = 24;
const HISTORY_ITEMS: usize = 50_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Waiting,
    Running,
    Stopping,
    Finished,
    Removed,
}

struct Job {
    id: u64,
    request: Arc<Request>,
    phase: Phase,
    progress: Progress,
    live: Vec<ItemState>,
    cancel: Option<Arc<AtomicBool>>,
    report: Option<Report>,
}

impl Job {
    fn status(&self) -> &'static str {
        match self.phase {
            Phase::Waiting => "Waiting",
            Phase::Running => "Running",
            Phase::Stopping => "Cancel requested",
            Phase::Removed => "Removed",
            Phase::Finished => {
                let report = self.report.as_ref().unwrap();
                if report.completed() == report.items.len() {
                    "Completed"
                } else if report.aborted {
                    "Canceled · check items"
                } else if report.failed {
                    "Failed · check items"
                } else {
                    "Check item results"
                }
            }
        }
    }

    fn terminal(&self) -> bool {
        matches!(self.phase, Phase::Finished | Phase::Removed)
    }
}

#[derive(Default)]
struct Queue {
    jobs: Vec<Job>,
    next_id: u64,
    paused: bool,
}

impl Queue {
    fn waiting(&self) -> usize {
        self.jobs
            .iter()
            .filter(|job| job.phase == Phase::Waiting)
            .count()
    }

    fn active(&self) -> Option<&Job> {
        self.jobs
            .iter()
            .find(|job| matches!(job.phase, Phase::Running | Phase::Stopping))
    }

    fn add(&mut self, request: Arc<Request>) {
        self.next_id += 1;
        self.jobs.push(Job {
            id: self.next_id,
            request,
            phase: Phase::Waiting,
            progress: Progress::default(),
            live: Vec::new(),
            cancel: None,
            report: None,
        });
    }

    fn start_next(&mut self) -> Option<(u64, Arc<Request>, Arc<AtomicBool>)> {
        if self.paused || self.active().is_some() {
            return None;
        }
        let job = self
            .jobs
            .iter_mut()
            .find(|job| job.phase == Phase::Waiting)?;
        let canceled = Arc::new(AtomicBool::new(false));
        job.phase = Phase::Running;
        job.live = vec![ItemState::Pending; job.request.files.len()];
        job.cancel = Some(canceled.clone());
        Some((job.id, job.request.clone(), canceled))
    }

    fn trim(&mut self) {
        let mut jobs = 0;
        let mut items = 0;
        let mut keep = vec![true; self.jobs.len()];
        for (index, job) in self.jobs.iter().enumerate().rev() {
            if job.terminal() {
                jobs += 1;
                items += job.request.files.len();
                keep[index] = jobs == 1 || (jobs <= HISTORY_JOBS && items <= HISTORY_ITEMS);
            }
        }
        let mut index = 0;
        self.jobs.retain(|_| {
            let retain = keep[index];
            index += 1;
            retain
        });
    }
}

#[derive(Default)]
pub(super) struct Transfers {
    queue: RefCell<Queue>,
    pub window: Cell<HWND>,
    selected: Cell<Option<u64>>,
    rendered: Cell<Option<u64>>,
    updating: Cell<bool>,
}

impl App {
    pub fn can_queue_transfer(&self) -> bool {
        self.transfers.queue.borrow().waiting() < LIMIT
    }

    pub fn enqueue_transfer(&self, request: Request) {
        if !self.can_queue_transfer() {
            message(
                self.window,
                "Transfer queue full",
                "Remove or finish a waiting transfer to make room.",
                MB_OK | MB_ICONINFORMATION,
            );
            return;
        }
        self.transfers.queue.borrow_mut().add(Arc::new(request));
        self.pump_transfers();
        self.transfer_ui_changed();
    }

    pub fn waiting_transfers(&self) -> usize {
        self.transfers.queue.borrow().waiting()
    }

    pub fn pump_transfers(&self) {
        // Other Shell file commands already use the same exclusive operation slot.
        if self.operation_active.get() {
            return;
        }
        let Some((id, request, cancel)) = self.transfers.queue.borrow_mut().start_next() else {
            return;
        };
        self.operation_active.set(true);
        let sender = self.sender.clone();
        let owner = self.window.0 as usize;
        let count = request.files.len();
        if let Err(error) = thread::Builder::new()
            .name("transfer".into())
            .spawn(move || {
                let progress_sender = sender.clone();
                let report = transfer::run(&request, owner, cancel, move |progress| {
                    if progress_sender
                        .send(Event::TransferProgress(id, progress))
                        .is_ok()
                    {
                        wake(owner);
                    }
                });
                if sender.send(Event::TransferFinished(id, report)).is_ok() {
                    wake(owner);
                }
            })
        {
            self.finish_transfer(
                id,
                Report::stopped(count, ItemState::NotStarted, error.to_string()),
            );
        }
    }

    pub fn transfer_progress(&self, id: u64, progress: ProgressUpdate) {
        if let Some(job) = self
            .transfers
            .queue
            .borrow_mut()
            .jobs
            .iter_mut()
            .find(|job| job.id == id && !job.terminal())
        {
            job.progress = progress.summary;
            for (index, state) in progress.items {
                if let Some(item) = job.live.get_mut(index) {
                    *item = state;
                }
            }
        }
        self.transfer_ui_changed();
    }

    pub fn finish_transfer(&self, id: u64, report: Report) {
        let mut queue = self.transfers.queue.borrow_mut();
        let Some(job) = queue
            .jobs
            .iter_mut()
            .find(|job| job.id == id && matches!(job.phase, Phase::Running | Phase::Stopping))
        else {
            return;
        };
        job.phase = Phase::Finished;
        job.live = Vec::new();
        job.cancel = None;
        let affected = job.request.paths.clone();
        let action = job.request.action;
        let note = format!("{} · {}", action.verb(), {
            job.report = Some(report);
            job.status()
        });
        queue.trim();
        drop(queue);
        self.operation_active.set(false);
        *self.operation_status.borrow_mut() = note;
        for (index, pane) in self.panes.iter().enumerate() {
            let refresh = {
                let listing = pane.listing.borrow();
                listing.path == affected[1]
                    || (matches!(action, Transfer::Move) && listing.path == affected[0])
            };
            if refresh {
                self.refresh(index);
            }
        }
        self.pump_transfers();
        self.transfer_ui_changed();
    }

    fn transfer_ui_changed(&self) {
        let queue = self.transfers.queue.borrow();
        let waiting = queue.waiting();
        let status = if let Some(job) = queue.active() {
            if job.phase == Phase::Stopping {
                Some(format!("Cancel requested · {waiting} waiting"))
            } else {
                Some(format!(
                    "{} running · {waiting} waiting",
                    job.request.action.verb()
                ))
            }
        } else if waiting != 0 {
            Some(format!(
                "{waiting} waiting · {}",
                if queue.paused { "Paused" } else { "Queued" }
            ))
        } else {
            None
        };
        let active = queue.active().is_some();
        drop(queue);
        if let Some(status) = status {
            *self.operation_status.borrow_mut() = status;
        }
        let text = if waiting > 0 || active {
            format!("Transfers ({})", waiting + usize::from(active))
        } else {
            "Transfers".to_owned()
        };
        set_text(self.transfer_button, &text);
        self.status_changed();
        let window = self.transfers.window.get();
        if !window.0.is_null() {
            self.render_transfers(window);
        }
    }

    fn pause_transfers(&self) {
        let mut queue = self.transfers.queue.borrow_mut();
        queue.paused = !queue.paused;
        if queue.waiting() == 0 && queue.active().is_none() {
            *self.operation_status.borrow_mut() = if queue.paused {
                "Transfer queue paused"
            } else {
                ""
            }
            .to_owned();
        }
        drop(queue);
        self.pump_transfers();
        self.transfer_ui_changed();
    }

    fn change_transfer(&self) {
        let mut queue = self.transfers.queue.borrow_mut();
        let Some(job) = queue
            .jobs
            .iter_mut()
            .find(|job| Some(job.id) == self.transfers.selected.get())
        else {
            return;
        };
        match job.phase {
            Phase::Waiting => {
                job.phase = Phase::Removed;
                job.report = Some(Report::stopped(
                    job.request.files.len(),
                    ItemState::Canceled,
                    "Removed before Windows started the job.".to_owned(),
                ));
                *self.operation_status.borrow_mut() = "Waiting transfer removed".to_owned();
            }
            Phase::Running => {
                job.phase = Phase::Stopping;
                job.cancel.as_ref().unwrap().store(true, Ordering::Relaxed);
            }
            _ => return,
        }
        queue.trim();
        drop(queue);
        self.transfer_ui_changed();
    }

    fn retry_transfer(&self) {
        let request = {
            let queue = self.transfers.queue.borrow();
            queue
                .jobs
                .iter()
                .find(|job| Some(job.id) == self.transfers.selected.get())
                .and_then(|job| {
                    job.report
                        .as_ref()
                        .and_then(|report| job.request.retry(report))
                })
        };
        let Some(request) = request else {
            return;
        };
        let prompt = format!(
            "Queue a {} retry for {} candidate files?\n\nFrom {}\nTo {}\n\nThe retry checks the original files again and requires empty destination names.",
            request.action.verb().to_lowercase(),
            grouped(request.files.len() as u64),
            request.paths[0].display(),
            request.paths[1].display(),
        );
        let owner = self.transfers.window.get();
        if message(
            owner,
            "Retry verified items",
            &prompt,
            MB_OKCANCEL | MB_ICONQUESTION | MB_DEFBUTTON2,
        ) == IDOK
        {
            self.enqueue_transfer(request);
        }
    }
}

fn set_text(window: HWND, label: &str) {
    let text = wide(label);
    unsafe {
        let _ = SetWindowTextW(window, PCWSTR(text.as_ptr()));
    }
}

const JOBS: u16 = 601;
const ITEMS: u16 = 602;
const SOURCE: u16 = 603;
const TARGET: u16 = 604;
const SUMMARY: u16 = 605;
const DETAIL: u16 = 606;
const BAR: u16 = 607;
const PAUSE: u16 = 608;
const CHANGE: u16 = 609;
const RETRY: u16 = 610;
const CLEAR_HISTORY: u16 = 611;
const OPEN_FOLDERS: u16 = 612;

impl App {
    pub fn show_transfers(&self) {
        let existing = self.transfers.window.get();
        if !existing.0.is_null() {
            unsafe {
                let _ = ShowWindow(existing, SW_RESTORE);
                let _ = SetForegroundWindow(existing);
            }
            return;
        }
        match prompt::create_panel(
            self.window,
            [505, 302],
            WINDOW_STYLE(0),
            Some(panel_proc),
            LPARAM((self as *const App) as isize),
        ) {
            Ok(window) if window_exists(window) => {
                self.transfers.window.set(window);
                self.render_transfers(window);
                unsafe {
                    let _ = ShowWindow(window, SW_SHOW);
                }
            }
            Ok(_) => {}
            Err(error) => {
                message(
                    self.window,
                    "Could not open transfers",
                    &error.message(),
                    MB_OK | MB_ICONWARNING,
                );
            }
        }
    }

    fn render_transfers(&self, window: HWND) {
        let queue = self.transfers.queue.borrow();
        let selected = self
            .transfers
            .selected
            .get()
            .and_then(|id| queue.jobs.iter().position(|job| job.id == id))
            .or_else(|| {
                queue
                    .active()
                    .and_then(|job| queue.jobs.iter().position(|row| row.id == job.id))
            })
            .or_else(|| {
                queue
                    .jobs
                    .iter()
                    .position(|job| job.phase == Phase::Waiting)
            })
            .or_else(|| queue.jobs.len().checked_sub(1));
        let job = selected.and_then(|row| queue.jobs.get(row));
        let id = job.map(|job| job.id);
        let count = queue.jobs.len();
        let item_count = job.map_or(0, |job| job.request.files.len());
        let folders = job.map(|job| job.request.paths.clone());
        let percentage = job.map_or(0, |job| {
            if job
                .report
                .as_ref()
                .is_some_and(|report| report.completed() == report.items.len())
            {
                100
            } else {
                job.progress.percent.unwrap_or(0)
            }
        });
        let paused = queue.paused;
        let change = job.and_then(|job| match job.phase {
            Phase::Waiting => Some("Remove waiting"),
            Phase::Running => Some("Cancel transfer"),
            Phase::Stopping => Some("Cancel requested"),
            _ => None,
        });
        let has_history = queue.jobs.iter().any(Job::terminal);
        let retry = self.can_queue_transfer()
            && job
                .and_then(|job| job.report.as_ref())
                .is_some_and(|report| report.retry_count() != 0);
        let summary = if count == 0 {
            "No transfers yet".to_owned()
        } else {
            format!(
                "{} · {} waiting",
                if paused {
                    "Queue paused"
                } else {
                    "Queue ready"
                },
                queue.waiting()
            )
        };
        let detail = job.map_or_else(String::new, |job| {
            if let Some(report) = &job.report {
                if !report.note.is_empty() {
                    report.note.replace('\n', " · ")
                } else if report.retry_count() != 0 {
                    format!(
                        "{} candidate files · retry rechecks the source and empty destination",
                        grouped(report.retry_count() as u64)
                    )
                } else if report.unconfirmed() {
                    "Open the folders and review unconfirmed items before trying them again."
                        .to_owned()
                } else {
                    job.status().to_owned()
                }
            } else {
                let progress = job.progress.percent.map_or_else(String::new, |pct| {
                    if matches!(job.request.action, Transfer::Extract) {
                        format!(" · Streaming {pct}%")
                    } else {
                        format!(" · Windows estimate {pct}%")
                    }
                });
                format!(
                    "{} · {} of {} confirmed{}",
                    job.status(),
                    grouped(job.progress.complete as u64),
                    grouped(job.request.files.len() as u64),
                    progress
                )
            }
        });
        drop(queue);
        let jobs = control(window, JOBS);
        let items = control(window, ITEMS);
        self.transfers.selected.set(id);
        let changed_job = self.transfers.rendered.replace(id) != id;
        let current = send(jobs, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize);
        if changed_job
            || send(jobs, LVM_GETITEMCOUNT, 0, 0) as usize != count
            || usize::try_from(current).ok() != selected
        {
            self.transfers.updating.set(true);
            send(jobs, LVM_SETITEMCOUNT, count, 0);
            item_state(
                jobs,
                usize::MAX,
                LIST_VIEW_ITEM_STATE_FLAGS(0),
                LVIS_SELECTED,
            );
            if let Some(row) = selected {
                item_state(jobs, row, LVIS_SELECTED, LVIS_SELECTED);
                send(jobs, LVM_ENSUREVISIBLE, row, 0);
            }
            self.transfers.updating.set(false);
        }
        if changed_job || send(items, LVM_GETITEMCOUNT, 0, 0) as usize != item_count {
            send(items, LVM_SETITEMCOUNT, item_count, 0);
        }
        if let Some(paths) = folders {
            for (id, path) in [SOURCE, TARGET].into_iter().zip(paths) {
                let field = control(window, id);
                if window_text(field) != path.as_os_str() {
                    show_path(field, &path);
                }
            }
        } else {
            set_text(control(window, SOURCE), "");
            set_text(control(window, TARGET), "");
        }
        set_text(control(window, SUMMARY), &summary);
        set_text(control(window, DETAIL), &detail);
        set_text(
            control(window, PAUSE),
            if paused {
                "&Resume queue"
            } else {
                "&Pause queue"
            },
        );
        set_text(control(window, CHANGE), change.unwrap_or("Cancel transfer"));
        send(control(window, BAR), PBM_SETPOS, percentage as usize, 0);
        unsafe {
            let _ = EnableWindow(
                control(window, CHANGE),
                change.is_some_and(|label| label != "Cancel requested"),
            );
            let _ = EnableWindow(control(window, RETRY), retry);
            let _ = EnableWindow(control(window, CLEAR_HISTORY), has_history);
            let _ = EnableWindow(control(window, OPEN_FOLDERS), id.is_some());
            let _ = InvalidateRect(Some(jobs), None, false);
            let _ = InvalidateRect(Some(items), None, false);
        }
    }

    fn choose_transfer(&self, window: HWND) {
        if self.transfers.updating.get() {
            return;
        }
        let index = send(
            control(window, JOBS),
            LVM_GETNEXTITEM,
            usize::MAX,
            LVNI_SELECTED as isize,
        );
        if index >= 0 {
            let id = self
                .transfers
                .queue
                .borrow()
                .jobs
                .get(index as usize)
                .map(|job| job.id);
            self.transfers.selected.set(id);
            self.render_transfers(window);
        }
    }

    fn transfer_text(&self, list: u16, item: &LVITEMW) {
        let queue = self.transfers.queue.borrow();
        if list == JOBS {
            if let Some(job) = queue.jobs.get(item.iItem as usize) {
                let text = match item.iSubItem {
                    0 => {
                        format!(
                            "{} {}{}",
                            job.request.action.verb(),
                            grouped(job.request.files.len() as u64),
                            if job.request.reviewed_copy() {
                                " · reviewed"
                            } else {
                                ""
                            }
                        )
                    }
                    1 => job.request.paths[1].display().to_string(),
                    _ => job.status().to_owned(),
                };
                copy_text(item, &wide(text));
                return;
            }
        } else if let Some(job) = queue
            .jobs
            .iter()
            .find(|job| Some(job.id) == self.transfers.selected.get())
            && let Some(file) = job.request.files.get(item.iItem as usize)
        {
            let report = job
                .report
                .as_ref()
                .and_then(|report| report.items.get(item.iItem as usize));
            match item.iSubItem {
                0 => copy_text(item, &wide(&file.name)),
                1 => copy_text(item, &wide(file.target())),
                2 => copy_text(
                    item,
                    &wide(report.map_or_else(
                        || {
                            job.live
                                .get(item.iItem as usize)
                                .copied()
                                .unwrap_or(ItemState::Pending)
                                .label()
                        },
                        |report| report.state.label(),
                    )),
                ),
                _ => copy_text(
                    item,
                    &wide(
                        report
                            .and_then(|report| report.detail.as_deref())
                            .unwrap_or(""),
                    ),
                ),
            }
            return;
        }
        copy_text(item, &[0]);
    }

    fn open_transfer_folders(&self) {
        let folders = self
            .transfers
            .queue
            .borrow()
            .jobs
            .iter()
            .find(|job| Some(job.id) == self.transfers.selected.get())
            .map(|job| (job.request.paths.clone(), job.request.zip_folder()));
        if let Some(([source, destination], zip_folder)) = folders {
            if let Some(folder) = zip_folder {
                self.navigate_zip(0, source, folder);
            } else {
                self.navigate(0, source);
            }
            self.navigate(1, destination);
            unsafe {
                let _ = SetForegroundWindow(self.window);
            }
            self.focus_list(self.active.get());
        }
    }
}

fn window_exists(window: HWND) -> bool {
    unsafe { IsWindow(Some(window)).as_bool() }
}

fn control(window: HWND, id: u16) -> HWND {
    unsafe { GetDlgItem(Some(window), id as i32) }.unwrap_or_default()
}

fn panel_initialize(app: &App, window: HWND) -> Result<()> {
    unsafe {
        SetWindowTextW(window, w!("Transfers"))?;
    }
    let normal = WINDOW_EX_STYLE::default();
    let edit = WINDOW_STYLE(ES_READONLY as u32 | ES_AUTOHSCROLL as u32);
    let list = WS_TABSTOP
        | WINDOW_STYLE(LVS_REPORT | LVS_OWNERDATA | LVS_SHOWSELALWAYS | LVS_NOSORTHEADER);
    let controls = [
        (
            w!("Static"),
            w!(""),
            normal,
            WINDOW_STYLE(0),
            SUMMARY,
            [9, 8, 399, 20],
        ),
        (
            w!("Button"),
            w!("&Open folders"),
            normal,
            WS_TABSTOP,
            OPEN_FOLDERS,
            [411, 4, 496, 20],
        ),
        (
            WC_LISTVIEWW,
            w!(""),
            WS_EX_CLIENTEDGE,
            list | WINDOW_STYLE(LVS_SINGLESEL),
            JOBS,
            [9, 22, 496, 97],
        ),
        (
            w!("Static"),
            w!("From"),
            normal,
            WINDOW_STYLE(0),
            0,
            [9, 105, 36, 119],
        ),
        (
            w!("Edit"),
            w!(""),
            WS_EX_CLIENTEDGE,
            edit,
            SOURCE,
            [37, 102, 496, 117],
        ),
        (
            w!("Static"),
            w!("To"),
            normal,
            WINDOW_STYLE(0),
            0,
            [9, 124, 36, 137],
        ),
        (
            w!("Edit"),
            w!(""),
            WS_EX_CLIENTEDGE,
            edit,
            TARGET,
            [37, 121, 496, 136],
        ),
        (
            WC_LISTVIEWW,
            w!(""),
            WS_EX_CLIENTEDGE,
            list,
            ITEMS,
            [9, 142, 496, 234],
        ),
        (
            PROGRESS_CLASSW,
            w!(""),
            normal,
            WINDOW_STYLE(0),
            BAR,
            [9, 240, 496, 249],
        ),
        (
            w!("Static"),
            w!(""),
            normal,
            WINDOW_STYLE(0),
            DETAIL,
            [9, 255, 496, 276],
        ),
        (
            w!("Button"),
            w!("&Pause queue"),
            normal,
            WS_TABSTOP,
            PAUSE,
            [9, 278, 93, 295],
        ),
        (
            w!("Button"),
            w!("Cancel transfer"),
            normal,
            WS_TABSTOP,
            CHANGE,
            [99, 278, 197, 295],
        ),
        (
            w!("Button"),
            w!("&Retry verified…"),
            normal,
            WS_TABSTOP,
            RETRY,
            [203, 278, 300, 295],
        ),
        (
            w!("Button"),
            w!("Clear &history"),
            normal,
            WS_TABSTOP,
            CLEAR_HISTORY,
            [306, 278, 388, 295],
        ),
        (
            w!("Button"),
            w!("Close"),
            normal,
            WS_TABSTOP,
            IDCANCEL.0 as u16,
            [433, 278, 496, 295],
        ),
    ];
    for template in controls {
        prompt::add_control(window, app.font.get(), template)?;
    }
    let setup = |id, columns: &[(PCWSTR, i32)]| {
        let list = control(window, id);
        send(list, CCM_SETUNICODEFORMAT, 1, 0);
        send(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            0,
            (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP) as isize,
        );
        let mut bounds = RECT::default();
        unsafe {
            let _ = GetClientRect(list, &mut bounds);
        }
        let width = bounds.right - app.scale(18);
        for (index, &(name, percent)) in columns.iter().enumerate() {
            let column = LVCOLUMNW {
                mask: LVCF_WIDTH | LVCF_TEXT | LVCF_FMT,
                fmt: LVCFMT_LEFT,
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
    };
    setup(
        JOBS,
        &[
            (w!("Transfer"), 22),
            (w!("Destination"), 50),
            (w!("Status"), 28),
        ],
    );
    setup(
        ITEMS,
        &[
            (w!("Source name"), 23),
            (w!("Destination name"), 23),
            (w!("Result"), 15),
            (w!("Detail"), 39),
        ],
    );
    send(control(window, BAR), PBM_SETRANGE32, 0, 100);
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
            if panel_initialize(app, window).is_err() {
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
                return 1;
            }
            WM_NCDESTROY => {
                app.transfers.window.set(HWND::default());
                app.transfers.rendered.set(None);
                SetWindowLongPtrW(window, GWLP_USERDATA, 0);
            }
            WM_COMMAND => {
                match wparam.0 as u16 {
                    PAUSE => app.pause_transfers(),
                    CHANGE => app.change_transfer(),
                    RETRY => app.retry_transfer(),
                    OPEN_FOLDERS => app.open_transfer_folders(),
                    CLEAR_HISTORY => {
                        app.transfers
                            .queue
                            .borrow_mut()
                            .jobs
                            .retain(|job| !job.terminal());
                        app.transfer_ui_changed();
                    }
                    command if command == IDCANCEL.0 as u16 => {
                        let _ = DestroyWindow(window);
                    }
                    _ => return 0,
                }
                return 1;
            }
            WM_NOTIFY => {
                let header = &*(lparam.0 as *const NMHDR);
                let id = header.idFrom as u16;
                if matches!(id, JOBS | ITEMS) {
                    match header.code {
                        LVN_GETDISPINFOW => {
                            let data = &*(lparam.0 as *const NMLVDISPINFOW);
                            if data.item.mask.contains(LVIF_TEXT) {
                                app.transfer_text(id, &data.item);
                            }
                        }
                        LVN_ITEMCHANGED | LVN_ODSTATECHANGED if id == JOBS => {
                            app.choose_transfer(window)
                        }
                        _ => {}
                    }
                }
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
    fn pauses_between_fifo_jobs() {
        let mut queue = Queue {
            paused: true,
            ..Queue::default()
        };
        for action in [Transfer::Copy, Transfer::Move] {
            queue.add(Arc::new(Request::direct(
                action,
                [
                    PathBuf::from(r"C:\source"),
                    PathBuf::from(r"C:\destination"),
                ],
                Vec::new(),
            )));
        }
        assert!(queue.start_next().is_none());
        queue.paused = false;
        let (first, request, _) = queue.start_next().unwrap();
        assert_eq!(first, 1);
        assert!(matches!(request.action, Transfer::Copy));
        assert!(queue.start_next().is_none());
        queue.jobs[0].phase = Phase::Finished;
        queue.paused = true;
        assert!(queue.start_next().is_none());
        queue.paused = false;
        let (second, request, _) = queue.start_next().unwrap();
        assert_eq!(second, 2);
        assert!(matches!(request.action, Transfer::Move));
    }
}
