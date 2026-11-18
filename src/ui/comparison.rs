use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
};

use super::*;
use crate::compare::{Comparison, Difference, Exact, Plan};

const RESULT: u32 = WM_APP + 2;
const LEFT_PATH: u16 = 521;
const RIGHT_PATH: u16 = 522;
const FILTER: u16 = 523;
const LIST: u16 = 524;
const STATUS: u16 = 525;
const SELECT: u16 = 526;
const EXACT: u16 = 527;
const OPEN_BOTH: u16 = 528;
const REVIEW_RIGHT: u16 = 529;
const REVIEW_LEFT: u16 = 530;
const REFRESH: u16 = 531;
const APPLY: u16 = 532;
const BACK: u16 = 533;
const CANCEL_JOB: u16 = 534;
const FILTER_LABEL: u16 = 535;
const REVIEW_HEADING: u16 = 536;
const CLOSE: u16 = IDCANCEL.0 as u16;
const COMPARE_COLUMNS: [PCWSTR; 4] = [w!("Name"), w!("Left"), w!("Right"), w!("Result")];
const REVIEW_COLUMNS: [PCWSTR; 4] = [
    w!("Source name"),
    w!("Destination name"),
    w!("Action"),
    w!("Review state"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
struct Revision {
    location: [usize; 2],
    changes: [usize; 2],
}

enum Outcome {
    Open([PathBuf; 2]),
    Copy(crate::transfer::Request),
}

enum Validation {
    Changed(Vec<usize>),
    Ready(crate::transfer::Request),
}

enum Feed {
    Comparison(Revision, io::Result<Comparison>),
    Exact(io::Result<Vec<(usize, Exact)>>),
    Prepared(io::Result<Plan>),
    Validation(io::Result<Validation>),
}

struct Review {
    plan: Arc<Plan>,
    changed: HashSet<usize>,
    stale: bool,
}

struct State {
    comparison: Option<Arc<Comparison>>,
    exact: Vec<Option<Exact>>,
    filter: Option<Difference>,
    shown: Vec<usize>,
    review: Option<Review>,
    baseline: Revision,
    busy: Option<Arc<AtomicBool>>,
}

impl State {
    fn count(&self) -> usize {
        if let Some(review) = &self.review {
            review.plan.rows.len()
        } else if self.filter.is_some() {
            self.shown.len()
        } else {
            self.comparison
                .as_ref()
                .map_or(0, |comparison| comparison.rows.len())
        }
    }

    fn row(&self, display: usize) -> Option<usize> {
        if let Some(review) = &self.review {
            review.plan.rows.get(display).copied()
        } else if self.filter.is_some() {
            self.shown.get(display).copied()
        } else {
            (display < self.count()).then_some(display)
        }
    }

    fn project(&mut self) {
        self.shown.clear();
        if let (Some(filter), Some(comparison)) = (self.filter, &self.comparison) {
            self.shown.extend(
                comparison
                    .rows
                    .iter()
                    .enumerate()
                    .filter_map(|(row, item)| (item.difference == filter).then_some(row)),
            );
        }
    }
}

struct Dialog<'a> {
    app: &'a App,
    paths: [PathBuf; 2],
    initial: RefCell<Option<[Arc<Vec<Entry>>; 2]>>,
    state: RefCell<State>,
    sender: Sender<Feed>,
    events: Receiver<Feed>,
    outcome: RefCell<Option<Outcome>>,
    adjusting_selection: Cell<bool>,
}

impl App {
    fn comparison_revision(&self) -> Revision {
        Revision {
            location: self
                .panes
                .each_ref()
                .map(|pane| pane.listing.borrow().location),
            changes: self
                .panes
                .each_ref()
                .map(|pane| pane.listing.borrow().filesystem_changes),
        }
    }

    pub(super) fn compare_folders(&self) {
        let [left, right] = self.panes.each_ref().map(|pane| pane.listing.borrow());
        if [&left, &right]
            .into_iter()
            .any(|listing| !listing.complete || listing.error.is_some())
        {
            message(
                self.window,
                "Compare folders",
                "Open two available folders and wait for their lists to finish loading.",
                MB_OK | MB_ICONINFORMATION,
            );
            return;
        }
        let paths = [left.path.clone(), right.path.clone()];
        let entries = [left.entries.clone(), right.entries.clone()];
        drop((left, right));
        let (sender, events) = mpsc::channel();
        let dialog = Dialog {
            app: self,
            paths,
            initial: RefCell::new(Some(entries)),
            state: RefCell::new(State {
                comparison: None,
                exact: Vec::new(),
                filter: None,
                shown: Vec::new(),
                review: None,
                baseline: self.comparison_revision(),
                busy: None,
            }),
            sender,
            events,
            outcome: RefCell::new(None),
            adjusting_selection: Cell::new(false),
        };
        let result = prompt::show_dialog(
            self.window,
            [530, 281],
            Some(dialog_proc),
            LPARAM((&dialog as *const Dialog) as isize),
        );
        if let Err(error) = result {
            message(
                self.window,
                "Could not open comparison",
                &error.message(),
                MB_OK | MB_ICONWARNING,
            );
        }
        match dialog.outcome.into_inner() {
            Some(Outcome::Open([left, right])) => {
                self.navigate(0, left);
                self.navigate(1, right);
                self.focus_list(self.active.get());
            }
            Some(Outcome::Copy(request)) => self.enqueue_transfer(request),
            None => {}
        }
    }
}

impl Dialog<'_> {
    fn initialize(&self, window: HWND) -> Result<()> {
        unsafe {
            SetWindowTextW(window, w!("Compare folders"))?;
        }
        let normal = WINDOW_EX_STYLE::default();
        let path = WINDOW_STYLE(ES_READONLY as u32 | ES_AUTOHSCROLL as u32);
        let controls = [
            (
                w!("Static"),
                w!("Left"),
                normal,
                WINDOW_STYLE(0),
                0,
                [9, 11, 36, 24],
            ),
            (
                w!("Edit"),
                w!(""),
                WS_EX_CLIENTEDGE,
                path,
                LEFT_PATH,
                [37, 8, 521, 23],
            ),
            (
                w!("Static"),
                w!("Right"),
                normal,
                WINDOW_STYLE(0),
                0,
                [9, 30, 36, 42],
            ),
            (
                w!("Edit"),
                w!(""),
                WS_EX_CLIENTEDGE,
                path,
                RIGHT_PATH,
                [37, 27, 521, 42],
            ),
            (
                w!("Static"),
                w!("Show"),
                normal,
                WINDOW_STYLE(0),
                FILTER_LABEL,
                [9, 51, 36, 64],
            ),
            (
                w!("Static"),
                w!(""),
                normal,
                WINDOW_STYLE(0),
                REVIEW_HEADING,
                [9, 51, 521, 65],
            ),
            (
                w!("ComboBox"),
                w!(""),
                normal,
                WS_VSCROLL | WS_TABSTOP | WINDOW_STYLE(CBS_DROPDOWNLIST as u32),
                FILTER,
                [37, 48, 172, 197],
            ),
            (
                w!("Button"),
                w!("Select shown files"),
                normal,
                WS_TABSTOP,
                SELECT,
                [179, 48, 275, 64],
            ),
            (
                WC_LISTVIEWW,
                w!(""),
                WS_EX_CLIENTEDGE,
                WS_TABSTOP
                    | WINDOW_STYLE(
                        LVS_REPORT | LVS_OWNERDATA | LVS_SHOWSELALWAYS | LVS_NOSORTHEADER,
                    ),
                LIST,
                [9, 70, 521, 231],
            ),
            (
                w!("Static"),
                w!(""),
                normal,
                WINDOW_STYLE(0),
                STATUS,
                [9, 236, 521, 253],
            ),
            (
                w!("Button"),
                w!("&Refresh"),
                normal,
                WS_TABSTOP,
                REFRESH,
                [9, 255, 61, 272],
            ),
            (
                w!("Button"),
                w!("Cancel work"),
                normal,
                WS_TABSTOP,
                CANCEL_JOB,
                [9, 255, 84, 272],
            ),
            (
                w!("Button"),
                w!("Compare contents"),
                normal,
                WS_TABSTOP,
                EXACT,
                [67, 255, 162, 272],
            ),
            (
                w!("Button"),
                w!("Open both folders"),
                normal,
                WS_TABSTOP,
                OPEN_BOTH,
                [168, 255, 260, 272],
            ),
            (
                w!("Button"),
                w!("Review to left"),
                normal,
                WS_TABSTOP,
                REVIEW_LEFT,
                [266, 255, 348, 272],
            ),
            (
                w!("Button"),
                w!("Review to right"),
                normal,
                WS_TABSTOP,
                REVIEW_RIGHT,
                [354, 255, 444, 272],
            ),
            (
                w!("Button"),
                w!("Back"),
                normal,
                WS_TABSTOP,
                BACK,
                [90, 255, 148, 272],
            ),
            (
                w!("Button"),
                w!("Apply copy"),
                normal,
                WS_TABSTOP,
                APPLY,
                [355, 255, 444, 272],
            ),
            (
                w!("Button"),
                w!("Close"),
                normal,
                WS_TABSTOP,
                CLOSE,
                [458, 255, 521, 272],
            ),
        ];
        for control in controls {
            prompt::add_control(window, self.app.font.get(), control)?;
        }
        for (id, folder) in [LEFT_PATH, RIGHT_PATH].into_iter().zip(&self.paths) {
            show_path(child(window, id), folder);
        }
        let list = child(window, LIST);
        send(list, CCM_SETUNICODEFORMAT, 1, 0);
        send(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            0,
            (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP) as isize,
        );
        let mut rect = RECT::default();
        unsafe {
            GetClientRect(list, &mut rect)?;
        }
        let width = rect.right - self.app.scale(18);
        for (index, (percent, label)) in [30, 23, 23, 24]
            .into_iter()
            .zip(COMPARE_COLUMNS)
            .enumerate()
        {
            let column = LVCOLUMNW {
                mask: LVCF_WIDTH | LVCF_TEXT | LVCF_FMT,
                cx: width * percent / 100,
                fmt: LVCFMT_LEFT,
                pszText: PWSTR(label.as_ptr().cast_mut()),
                ..Default::default()
            };
            send(
                list,
                LVM_INSERTCOLUMNW,
                index,
                (&column as *const LVCOLUMNW) as isize,
            );
        }
        let combo = child(window, FILTER);
        for label in std::iter::once("All entries").chain(Difference::ALL.map(Difference::label)) {
            let text = wide(label);
            send(combo, CB_ADDSTRING, 0, text.as_ptr() as isize);
        }
        send(combo, CB_SETCURSEL, 0, 0);
        let initial = self.initial.borrow_mut().take().unwrap();
        let paths = self.paths.clone();
        let revision = self.state.borrow().baseline;
        self.start(
            window,
            "Comparing loaded folder entries…",
            move |current| Feed::Comparison(revision, Comparison::build(paths, initial, current)),
        );
        unsafe {
            let _ = SetFocus(Some(child(window, LIST)));
        }
        Ok(())
    }

    fn start<F>(&self, window: HWND, status: &str, work: F)
    where
        F: FnOnce(&dyn Fn() -> bool) -> Feed + Send + 'static,
    {
        if self.state.borrow().busy.is_some() {
            return;
        }
        let guard = self.app.compare_worker.clone();
        if guard.swap(true, Ordering::AcqRel) {
            self.set_status(
                window,
                "An earlier comparison is waiting for Windows. Try Refresh once it stops.",
            );
            self.buttons(window);
            return;
        }
        let canceled = Arc::new(AtomicBool::new(false));
        self.state.borrow_mut().busy = Some(canceled.clone());
        self.set_status(window, status);
        self.buttons(window);
        let owner = window.0 as usize;
        let sender = self.sender.clone();
        let release = guard.clone();
        let spawn = thread::Builder::new()
            .name("folder-comparison".into())
            .spawn(move || {
                let feed = work(&|| !canceled.load(Ordering::Relaxed));
                release.store(false, Ordering::Release);
                if sender.send(feed).is_ok() {
                    unsafe {
                        let _ = PostMessageW(Some(HWND(owner as _)), RESULT, WPARAM(0), LPARAM(0));
                    }
                }
            });
        if let Err(error) = spawn {
            guard.store(false, Ordering::Release);
            self.state.borrow_mut().busy = None;
            self.set_status(window, &error.to_string());
            self.buttons(window);
        }
    }

    fn receive(&self, window: HWND) {
        while let Ok(feed) = self.events.try_recv() {
            let canceled = self
                .state
                .borrow_mut()
                .busy
                .take()
                .is_some_and(|cancel| cancel.load(Ordering::Relaxed));
            if canceled {
                self.set_status(window, "Comparison work canceled.");
                self.buttons(window);
                continue;
            }
            match feed {
                Feed::Comparison(revision, Ok(result)) => {
                    let mut state = self.state.borrow_mut();
                    state.comparison = Some(Arc::new(result));
                    state.baseline = revision;
                    state.review = None;
                    state.exact.clear();
                    state.project();
                    drop(state);
                    self.columns(window, false);
                    self.populate(window);
                    self.show_count(window);
                }
                Feed::Exact(Ok(results)) => {
                    if results.is_empty() {
                        self.set_status(window, "Select one or more matched regular-file pairs to compare their contents.");
                        self.buttons(window);
                        continue;
                    }
                    let equal = results
                        .iter()
                        .filter(|(_, value)| *value == Exact::Equal)
                        .count();
                    let different = results
                        .iter()
                        .filter(|(_, value)| *value == Exact::Different)
                        .count();
                    let other = results.len() - equal - different;
                    let mut state = self.state.borrow_mut();
                    let count = state.comparison.as_ref().unwrap().rows.len();
                    state.exact.resize(count, None);
                    for (row, value) in results {
                        state.exact[row] = Some(value);
                    }
                    drop(state);
                    self.set_status(
                        window,
                        &format!(
                            "{} byte-equal · {} different · {} changed or unreadable",
                            grouped(equal as u64),
                            grouped(different as u64),
                            grouped(other as u64)
                        ),
                    );
                    invalidate(child(window, LIST));
                }
                Feed::Prepared(Ok(plan)) => self.show_review(window, plan),
                Feed::Validation(Ok(validation)) => {
                    // Include any watcher messages already waiting on the owner's queue.
                    self.app.events();
                    let current = self.app.comparison_revision();
                    let mut state = self.state.borrow_mut();
                    let revision_changed = current != state.baseline;
                    let review = state.review.as_mut().unwrap();
                    if matches!(validation, Validation::Changed(_)) || revision_changed {
                        review.stale = true;
                        if let Validation::Changed(changed) = validation {
                            review.changed.extend(changed);
                        }
                        let text = if review.changed.is_empty() {
                            "A folder changed during review. Refresh and review again.".to_owned()
                        } else {
                            format!(
                                "{} changed. Refresh and review again.",
                                quantity(
                                    review.changed.len(),
                                    "reviewed entry",
                                    "reviewed entries"
                                )
                            )
                        };
                        drop(state);
                        self.set_status(window, &text);
                        invalidate(child(window, LIST));
                    } else if !self.app.can_queue_transfer() {
                        drop(state);
                        self.set_status(
                            window,
                            "The transfer queue is full. Remove or finish a waiting transfer and try again.",
                        );
                    } else if let Validation::Ready(request) = validation {
                        self.outcome.replace(Some(Outcome::Copy(request)));
                        drop(state);
                        unsafe {
                            let _ = EndDialog(window, 1);
                        }
                        return;
                    }
                }
                Feed::Validation(Err(error)) => {
                    if let Some(review) = &mut self.state.borrow_mut().review {
                        review.stale = true;
                    }
                    self.set_status(window, &format!("Could not verify the reviewed folders. Refresh and review again. {error}"));
                    invalidate(child(window, LIST));
                }
                Feed::Comparison(_, Err(error))
                | Feed::Exact(Err(error))
                | Feed::Prepared(Err(error)) => {
                    self.set_status(window, &error.to_string());
                }
            }
            self.buttons(window);
        }
    }

    fn commands(&self, window: HWND, id: u16, code: u16) {
        if id == CLOSE {
            self.cancel();
            unsafe {
                let _ = EndDialog(window, 0);
            }
            return;
        }
        if id == CANCEL_JOB {
            self.cancel();
            self.set_status(window, "Canceling… waiting for the current read to return.");
            return;
        }
        if self.state.borrow().busy.is_some() {
            return;
        }
        match id {
            FILTER if code == CBN_SELCHANGE as u16 => {
                let index = send(child(window, FILTER), CB_GETCURSEL, 0, 0);
                let mut state = self.state.borrow_mut();
                state.filter = Difference::ALL.get((index - 1) as usize).copied();
                state.project();
                drop(state);
                self.populate(window);
                self.show_count(window);
            }
            SELECT => self.select_shown_files(window),
            REFRESH => {
                let paths = self.paths.clone();
                // Refresh replaces the snapshot only after both folders are read.
                let revision = self.app.comparison_revision();
                self.start(window, "Reading both folders…", move |current| {
                    Feed::Comparison(revision, Comparison::refresh(paths, current))
                });
                // A failed or canceled refresh must not bless an old review.
                let mut state = self.state.borrow_mut();
                if state.busy.is_some()
                    && let Some(review) = &mut state.review
                {
                    review.stale = true;
                }
            }
            EXACT => self.exact(window),
            OPEN_BOTH => self.open_both(window),
            REVIEW_LEFT => self.review(window, 1),
            REVIEW_RIGHT => self.review(window, 0),
            BACK => {
                self.state.borrow_mut().review = None;
                self.columns(window, false);
                self.populate(window);
                self.show_count(window);
            }
            APPLY => self.apply(window),
            _ => {}
        }
        self.buttons(window);
    }

    fn selected(&self, window: HWND) -> Vec<usize> {
        let state = self.state.borrow();
        let mut previous = -1;
        let list = child(window, LIST);
        std::iter::from_fn(|| {
            previous = send(
                list,
                LVM_GETNEXTITEM,
                previous as usize,
                LVNI_SELECTED as isize,
            );
            usize::try_from(previous)
                .ok()
                .and_then(|row| state.row(row))
        })
        .collect()
    }

    fn select_shown_files(&self, window: HWND) {
        let state = self.state.borrow();
        let Some(comparison) = &state.comparison else {
            return;
        };
        if state.review.is_some() {
            return;
        }
        let count = state.count();
        let rows = (0..count)
            .filter(|&display| {
                state
                    .row(display)
                    .is_some_and(|row| comparison.eligible_either_way(row))
            })
            .collect::<Vec<_>>();
        drop(state);
        let list = child(window, LIST);
        self.adjusting_selection.set(true);
        if rows.len() == count {
            item_state(list, usize::MAX, LVIS_SELECTED, LVIS_SELECTED);
        } else {
            item_state(
                list,
                usize::MAX,
                LIST_VIEW_ITEM_STATE_FLAGS(0),
                LVIS_SELECTED,
            );
            for row in rows {
                item_state(list, row, LVIS_SELECTED, LVIS_SELECTED);
            }
        }
        self.adjusting_selection.set(false);
        unsafe {
            let _ = SetFocus(Some(list));
        }
    }

    fn exact(&self, window: HWND) {
        let selected = self.selected(window);
        let state = self.state.borrow();
        if state.review.is_some() {
            return;
        }
        let Some(comparison) = state.comparison.as_ref().cloned() else {
            return;
        };
        drop(state);
        if selected.is_empty() {
            self.set_status(
                window,
                "Select one or more matched regular-file pairs to compare their contents.",
            );
            return;
        }
        self.start(
            window,
            "Comparing the selected file pairs…",
            move |current| Feed::Exact(comparison.contents(&selected, current)),
        );
    }

    fn open_both(&self, window: HWND) {
        let selected = self.selected(window);
        let state = self.state.borrow();
        let Some(comparison) = &state.comparison else {
            return;
        };
        if let [row] = selected.as_slice()
            && comparison.rows[*row].difference == Difference::BothFolders
            && state.review.is_none()
        {
            let paths = std::array::from_fn(|side| {
                comparison.paths[side].join(comparison.entry(*row, side).unwrap().filename())
            });
            self.outcome.replace(Some(Outcome::Open(paths)));
            drop(state);
            unsafe {
                let _ = EndDialog(window, 1);
            }
        } else {
            drop(state);
            self.set_status(
                window,
                "Select one matching folder to open it in both panes.",
            );
        }
    }

    fn review(&self, window: HWND, source: usize) {
        let rows = self.selected(window);
        let state = self.state.borrow();
        let Some(comparison) = state.comparison.as_ref().cloned() else {
            return;
        };
        if rows.is_empty() || state.review.is_some() {
            return;
        }
        drop(state);
        self.start(window, "Checking the two folder locations…", move |_| {
            Feed::Prepared(comparison.prepare(source, rows))
        });
    }

    fn show_review(&self, window: HWND, plan: Plan) {
        let mut state = self.state.borrow_mut();
        let comparison = state.comparison.as_ref().unwrap();
        let source = plan.source;
        let replacements = plan
            .rows
            .iter()
            .filter(|&&row| comparison.entry(row, 1 - source).is_some())
            .count();
        let count = plan.rows.len();
        state.review = Some(Review {
            plan: Arc::new(plan),
            changed: HashSet::new(),
            stale: false,
        });
        drop(state);
        self.columns(window, true);
        self.populate(window);
        let direction = if source == 0 {
            "left → right"
        } else {
            "right → left"
        };
        let heading = wide(format!("Review copy {direction}"));
        unsafe {
            let _ = SetWindowTextW(child(window, REVIEW_HEADING), PCWSTR(heading.as_ptr()));
        }
        self.set_status(
            window,
            &format!(
                "Copy {direction} · {} · {}",
                quantity(count, "file", "files"),
                quantity(replacements, "replacement", "replacements")
            ),
        );
    }

    fn apply(&self, window: HWND) {
        let state = self.state.borrow();
        let Some(review) = state.review.as_ref().filter(|review| !review.stale) else {
            return;
        };
        if !self.app.can_queue_transfer() {
            drop(state);
            self.set_status(
                window,
                "The transfer queue is full. Remove or finish a waiting transfer and try again.",
            );
            return;
        }
        let comparison = state.comparison.as_ref().unwrap().clone();
        let plan = review.plan.clone();
        drop(state);
        self.start(
            window,
            "Checking both folders against the reviewed files…",
            move |current| {
                Feed::Validation(comparison.validate(&plan, current).map(|changed| {
                    if changed.is_empty() {
                        Validation::Ready(crate::transfer::Request::reviewed(&comparison, &plan))
                    } else {
                        Validation::Changed(changed)
                    }
                }))
            },
        );
    }

    fn cancel(&self) {
        if let Some(cancel) = &self.state.borrow().busy {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    fn populate(&self, window: HWND) {
        let list = child(window, LIST);
        item_state(
            list,
            usize::MAX,
            LIST_VIEW_ITEM_STATE_FLAGS(0),
            LVIS_SELECTED,
        );
        let count = self.state.borrow().count();
        send(list, LVM_SETITEMCOUNT, count, 0);
        scroll_to(list, 0);
        invalidate(list);
    }

    fn show_count(&self, window: HWND) {
        let state = self.state.borrow();
        let count = grouped(state.count() as u64);
        let total = grouped(
            state
                .comparison
                .as_ref()
                .map_or(0, |comparison| comparison.rows.len()) as u64,
        );
        drop(state);
        self.set_status(
            window,
            &format!("{count} of {total} entries · immediate folder contents"),
        );
    }

    fn set_status(&self, window: HWND, status: &str) {
        let text = wide(status);
        unsafe {
            let _ = SetWindowTextW(child(window, STATUS), PCWSTR(text.as_ptr()));
        }
    }

    fn buttons(&self, window: HWND) {
        let (review, busy, can_apply) = {
            let state = self.state.borrow();
            (
                state.review.is_some(),
                state.busy.is_some(),
                state.review.as_ref().is_some_and(|r| !r.stale),
            )
        };
        let selected = send(child(window, LIST), LVM_GETSELECTEDCOUNT, 0, 0) as usize;
        let has_rows = self.state.borrow().count() > 0;
        for (id, show, enable) in [
            (FILTER_LABEL, !review, true),
            (REVIEW_HEADING, review, true),
            (FILTER, !review, !busy),
            (SELECT, !review, !busy && has_rows),
            (EXACT, !review, !busy && selected > 0),
            (OPEN_BOTH, !review, !busy && selected == 1),
            (REVIEW_LEFT, !review, !busy && selected > 0),
            (REVIEW_RIGHT, !review, !busy && selected > 0),
            (REFRESH, !busy, !busy),
            (CANCEL_JOB, busy, busy),
            (BACK, review, !busy),
            (APPLY, review, !busy && can_apply),
        ] {
            let control = child(window, id);
            unsafe {
                let _ = ShowWindow(control, if show { SW_SHOWNA } else { SW_HIDE });
                let _ = EnableWindow(control, enable);
            }
        }
    }

    fn columns(&self, window: HWND, review: bool) {
        let list = child(window, LIST);
        let names = if review {
            REVIEW_COLUMNS
        } else {
            COMPARE_COLUMNS
        };
        for (index, label) in names.into_iter().enumerate() {
            let column = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_FMT,
                fmt: LVCFMT_LEFT,
                pszText: PWSTR(label.as_ptr().cast_mut()),
                ..Default::default()
            };
            send(
                list,
                LVM_SETCOLUMNW,
                index,
                (&column as *const LVCOLUMNW) as isize,
            );
        }
    }

    fn text(&self, item: &LVITEMW) {
        let state = self.state.borrow();
        let Some((comparison, row)) = state
            .comparison
            .as_ref()
            .zip(state.row(item.iItem as usize))
        else {
            copy_text(item, &[0]);
            return;
        };
        if let Some(review) = &state.review {
            let source = comparison.entry(row, review.plan.source).unwrap();
            let destination = comparison.entry(row, 1 - review.plan.source);
            match item.iSubItem {
                0 => copy_text(item, &source.name),
                1 => copy_text(item, &destination.unwrap_or(source).name),
                2 => copy_text(
                    item,
                    &wide(if destination.is_some() {
                        "Replace"
                    } else {
                        "Create"
                    }),
                ),
                _ => {
                    let text = if review.changed.contains(&row) {
                        "Changed"
                    } else if review.stale {
                        "Refresh required"
                    } else {
                        ""
                    };
                    copy_text(item, &wide(text));
                }
            }
        } else {
            match item.iSubItem {
                0 => copy_text(item, comparison.name(row)),
                1 | 2 => {
                    let details = comparison
                        .entry(row, item.iSubItem as usize - 1)
                        .map_or_else(
                            || "—".to_owned(),
                            |entry| {
                                if entry.directory {
                                    "Folder".to_owned()
                                } else {
                                    format!("{} · {}", entry.detail(1), entry.detail(2))
                                }
                            },
                        );
                    copy_text(item, &wide(details));
                }
                _ => {
                    let metadata = comparison.rows[row].difference.label();
                    let text = state
                        .exact
                        .get(row)
                        .copied()
                        .flatten()
                        .map_or(metadata, Exact::label);
                    copy_text(item, &wide(text));
                }
            }
        }
    }
}

fn child(window: HWND, id: u16) -> HWND {
    unsafe { GetDlgItem(Some(window), id as i32) }.unwrap_or_default()
}

fn quantity(count: usize, singular: &str, plural: &str) -> String {
    let noun = if count == 1 { singular } else { plural };
    format!("{} {noun}", grouped(count as u64))
}

fn invalidate(window: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(window), None, false);
    }
}

unsafe extern "system" fn dialog_proc(
    window: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    unsafe {
        if msg == WM_INITDIALOG {
            SetWindowLongPtrW(window, GWLP_USERDATA, lparam.0);
            let dialog = &*(lparam.0 as *const Dialog);
            if dialog.initialize(window).is_err() {
                let _ = EndDialog(window, -1);
            }
            return 0;
        }
        let Some(dialog) = (GetWindowLongPtrW(window, GWLP_USERDATA) as *const Dialog).as_ref()
        else {
            return 0;
        };
        match msg {
            WM_CLOSE => {
                dialog.commands(window, CLOSE, 0);
                return 1;
            }
            WM_COMMAND => {
                dialog.commands(window, wparam.0 as u16, (wparam.0 >> 16) as u16);
                return 1;
            }
            RESULT => {
                dialog.receive(window);
                return 1;
            }
            WM_NOTIFY => {
                let notification = &*(lparam.0 as *const NMHDR);
                if notification.idFrom == LIST as usize {
                    match notification.code {
                        LVN_GETDISPINFOW => {
                            let request = &*(lparam.0 as *const NMLVDISPINFOW);
                            if request.item.mask.contains(LVIF_TEXT) {
                                dialog.text(&request.item);
                            }
                        }
                        LVN_ITEMCHANGED | LVN_ODSTATECHANGED
                            if !dialog.adjusting_selection.get() =>
                        {
                            dialog.buttons(window)
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
