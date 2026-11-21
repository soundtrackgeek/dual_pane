use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    io,
    os::windows::ffi::OsStringExt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{E_FAIL, ERROR_CANCELLED, HWND, S_OK},
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree},
        UI::Shell::{
            COPYENGINE_E_CANCELLED, COPYENGINE_E_USER_CANCELLED,
            COPYENGINE_S_DONT_PROCESS_CHILDREN, COPYENGINE_S_USER_IGNORED, FOF_NOCONFIRMMKDIR,
            FOFX_ADDUNDORECORD, FileOperation, IFileOperation, IFileOperationProgressSink,
            IFileOperationProgressSink_Impl, IShellItem, SIGDN_FILESYSPATH,
        },
    },
    core::{Error, HRESULT, PCWSTR, Ref, Result, implement},
};

use crate::{
    archive,
    compare::{self, Comparison, Identity, Plan, Stamp},
    directory::{Entry, grouped, wide},
    shell::{self, Transfer},
    undo,
};

enum Policy {
    Direct,
    Reviewed([Identity; 2]),
    Retry([Identity; 2]),
    Extract(archive::Source),
    Pack(OsString),
    Incoming { clipboard: Option<u32>, mixed: bool },
}

pub struct Request {
    pub action: Transfer,
    pub paths: [PathBuf; 2],
    pub files: Vec<Requested>,
    policy: Policy,
}

#[derive(Clone)]
pub struct Requested {
    pub name: OsString,
    target: Option<OsString>,
    stamp: Option<Stamp>,
    path: Option<PathBuf>,
    replacement: Option<Stamp>,
    identity: Option<Identity>,
}

impl Requested {
    pub fn selected(entry: &Entry) -> Self {
        Self {
            name: entry.filename(),
            target: None,
            stamp: Some(Stamp::entry(entry)),
            path: None,
            replacement: None,
            identity: None,
        }
    }

    pub fn target(&self) -> &OsStr {
        self.target.as_deref().unwrap_or(&self.name)
    }
}

impl Request {
    pub fn incoming(
        action: Transfer,
        destination: PathBuf,
        paths: Vec<PathBuf>,
        clipboard: Option<u32>,
    ) -> Option<Self> {
        let source = paths.first()?.parent()?.to_owned();
        let mixed = paths
            .iter()
            .any(|path| path.parent() != Some(source.as_path()));
        let files = paths
            .into_iter()
            .filter_map(|path| {
                Some(Requested {
                    name: path.file_name()?.to_owned(),
                    target: None,
                    stamp: None,
                    path: Some(path),
                    replacement: None,
                    identity: None,
                })
            })
            .collect();
        Some(Self {
            action,
            paths: [source, destination],
            files,
            policy: Policy::Incoming { clipboard, mixed },
        })
    }

    pub fn clipboard_sequence(&self) -> Option<u32> {
        if let Policy::Incoming { clipboard, .. } = self.policy {
            clipboard
        } else {
            None
        }
    }

    pub fn source_path(&self, file: &Requested) -> PathBuf {
        file.path
            .clone()
            .unwrap_or_else(|| self.paths[0].join(&file.name))
    }

    pub fn refresh_source(&self, path: &std::path::Path) -> bool {
        if !matches!(self.action, Transfer::Move) {
            return false;
        }
        if self.incoming_transfer() {
            self.files
                .iter()
                .any(|file| file.path.as_deref().and_then(std::path::Path::parent) == Some(path))
        } else {
            path == self.paths[0]
        }
    }

    pub fn incoming_transfer(&self) -> bool {
        matches!(self.policy, Policy::Incoming { .. })
    }

    pub fn mixed_sources(&self) -> bool {
        matches!(self.policy, Policy::Incoming { mixed: true, .. })
    }

    pub fn extract(paths: [PathBuf; 2], files: Vec<Requested>, source: archive::Source) -> Self {
        Self {
            action: Transfer::Extract,
            paths,
            files,
            policy: Policy::Extract(source),
        }
    }

    pub fn pack(paths: [PathBuf; 2], files: Vec<Requested>, archive_name: OsString) -> Self {
        Self {
            action: Transfer::Pack,
            paths,
            files,
            policy: Policy::Pack(archive_name),
        }
    }

    pub fn zip_folder(&self) -> Option<Arc<str>> {
        if let Policy::Extract(source) = &self.policy {
            Some(source.folder.clone())
        } else {
            None
        }
    }
    pub fn direct(action: Transfer, paths: [PathBuf; 2], files: Vec<Requested>) -> Self {
        Self {
            action,
            paths,
            files,
            policy: Policy::Direct,
        }
    }

    pub fn reviewed(comparison: &Comparison, plan: &Plan) -> Self {
        let source = plan.source;
        Self {
            action: Transfer::Copy,
            paths: [
                comparison.paths[source].clone(),
                comparison.paths[1 - source].clone(),
            ],
            files: comparison
                .copy_items(plan)
                .map(|item| Requested {
                    name: item.source.file_name().unwrap().to_os_string(),
                    target: Some(item.destination_name),
                    stamp: Some(item.original),
                    path: None,
                    replacement: item.replacement,
                    identity: None,
                })
                .collect(),
            policy: Policy::Reviewed(plan.oriented_roots()),
        }
    }

    pub fn reviewed_copy(&self) -> bool {
        matches!(self.policy, Policy::Reviewed(_))
    }

    pub fn retry(&self, report: &Report) -> Option<Self> {
        let roots = report.roots?;
        let files = self
            .files
            .iter()
            .zip(&report.items)
            .filter_map(|(file, old)| {
                let identity = old.retry_identity?;
                Some(Requested {
                    identity: Some(identity),
                    replacement: None,
                    ..file.clone()
                })
            })
            .collect::<Vec<_>>();
        (!files.is_empty()).then(|| Self {
            action: self.action,
            paths: self.paths.clone(),
            files,
            policy: Policy::Retry(roots),
        })
    }

    fn identities(&self) -> io::Result<[Identity; 2]> {
        let roots = [
            compare::directory_identity(&self.paths[0])?,
            compare::directory_identity(&self.paths[1])?,
        ];
        if roots[0] == roots[1] {
            return Err(io::Error::other(
                "The source and destination resolve to the same folder.",
            ));
        }
        match self.policy {
            Policy::Reviewed(expected) | Policy::Retry(expected) if expected != roots => {
                Err(io::Error::other(
                    "A saved path now points to a different folder. Review the folders again.",
                ))
            }
            _ => Ok(roots),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ItemState {
    Pending,
    Copied,
    Moved,
    Extracted,
    Packed,
    Skipped,
    Failed,
    Canceled,
    NotStarted,
    Unconfirmed,
}

impl ItemState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Copied => "Copied",
            Self::Moved => "Moved",
            Self::Extracted => "Extracted",
            Self::Packed => "Packed",
            Self::Skipped => "Skipped",
            Self::Failed => "Failed",
            Self::Canceled => "Canceled",
            Self::NotStarted => "Not started",
            Self::Unconfirmed => "Unconfirmed",
        }
    }

    pub fn completed(self) -> bool {
        matches!(
            self,
            Self::Copied | Self::Moved | Self::Extracted | Self::Packed
        )
    }
}

#[derive(Clone)]
pub struct ItemReport {
    pub state: ItemState,
    pub detail: Option<String>,
    retry_identity: Option<Identity>,
}

pub struct Report {
    pub items: Vec<ItemReport>,
    pub note: String,
    pub aborted: bool,
    pub failed: bool,
    pub undo: Vec<undo::Receipt>,
    roots: Option<[Identity; 2]>,
    completed: usize,
    retryable: usize,
    unconfirmed: bool,
}

impl Report {
    pub fn empty(count: usize) -> Self {
        Self {
            items: vec![
                ItemReport {
                    state: ItemState::Pending,
                    detail: None,
                    retry_identity: None
                };
                count
            ],
            note: String::new(),
            aborted: false,
            failed: false,
            undo: Vec::new(),
            roots: None,
            completed: 0,
            retryable: 0,
            unconfirmed: false,
        }
    }

    pub fn stopped(count: usize, state: ItemState, note: String) -> Self {
        let mut report = Self::empty(count);
        report.items.iter_mut().for_each(|item| item.state = state);
        report.note = note;
        report.aborted = state == ItemState::Canceled;
        report.failed = state == ItemState::NotStarted;
        report
    }

    pub fn retry_count(&self) -> usize {
        self.retryable
    }

    pub fn completed(&self) -> usize {
        self.completed
    }

    pub fn unconfirmed(&self) -> bool {
        self.unconfirmed
    }
}

#[derive(Clone, Copy, Default)]
pub struct Progress {
    pub percent: Option<u32>,
    pub complete: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub eta_seconds: Option<u64>,
}

fn streaming_progress(complete: usize, bytes: u128, total: u128, elapsed: Duration) -> Progress {
    let percent = (total > 0).then(|| (bytes * 100 / total).min(99) as u32);
    let eta_seconds = if total > bytes && bytes > 0 && elapsed >= Duration::from_secs(1) {
        let rate = bytes as f64 / elapsed.as_secs_f64();
        if rate > 0.0 {
            Some(((total - bytes) as f64 / rate).ceil() as u64)
        } else {
            None
        }
    } else {
        None
    };
    Progress {
        percent,
        complete,
        bytes_done: bytes.min(u128::from(u64::MAX)) as u64,
        bytes_total: total.min(u128::from(u64::MAX)) as u64,
        eta_seconds,
    }
}

pub struct ProgressUpdate {
    pub summary: Progress,
    pub items: Vec<(usize, ItemState)>,
}

struct Tracker {
    action: Transfer,
    paths: HashMap<PathBuf, usize>,
    undo_candidates: HashMap<usize, undo::Candidate>,
    report: Report,
    cancel: Arc<AtomicBool>,
    send: Box<dyn Fn(ProgressUpdate) + Send>,
    progress: Progress,
    changes: Vec<(usize, ItemState)>,
    last: Instant,
    invoked: bool,
}

impl Tracker {
    fn tick(&mut self) {
        if self.last.elapsed() >= Duration::from_millis(120) {
            self.last = Instant::now();
            (self.send)(ProgressUpdate {
                summary: self.progress,
                items: std::mem::take(&mut self.changes),
            });
        }
    }

    fn record(&mut self, index: usize, state: ItemState, detail: Option<String>) {
        let result = &mut self.report.items[index];
        if result.state.completed() != state.completed() {
            if result.state.completed() {
                self.progress.complete -= 1;
            } else {
                self.progress.complete += 1;
            }
        }
        if result.state != state {
            self.changes.push((index, state));
            result.state = state;
        }
        result.detail = detail;
        self.tick();
    }
}

#[implement(IFileOperationProgressSink)]
struct Sink(Arc<Mutex<Tracker>>);

impl Sink_Impl {
    fn check_cancel(&self) -> Result<()> {
        canceled(&self.0.lock().unwrap().cancel)
    }

    fn before(&self, item: Option<&IShellItem>) -> Result<()> {
        if let Err(error) = self.check_cancel() {
            let path = shell_path(item);
            let mut tracker = self.0.lock().unwrap();
            if let Some(index) = path
                .as_ref()
                .and_then(|path| tracker.paths.get(path).copied())
            {
                tracker.record(index, ItemState::Canceled, None);
            }
            return Err(error);
        }
        Ok(())
    }

    fn after(
        &self,
        item: Ref<IShellItem>,
        result: HRESULT,
        destination: Ref<IShellItem>,
    ) -> Result<()> {
        let path = shell_path(item.as_ref());
        let actual = if matches!(result, S_OK | COPYENGINE_S_DONT_PROCESS_CHILDREN) {
            shell_path(destination.as_ref())
        } else {
            None
        };
        let mut tracker = self.0.lock().unwrap();
        tracker.invoked = true;
        let index = path
            .as_ref()
            .and_then(|path| tracker.paths.get(path).copied());
        if let Some(index) = index {
            let state = classify(tracker.action, result, destination.as_ref().is_some());
            if let Some(candidate) = tracker.undo_candidates.remove(&index)
                && state == ItemState::Moved
                && let Some(receipt) = actual.as_deref().and_then(|path| candidate.confirm(path))
            {
                tracker.report.undo.push(receipt);
            }
            let detail = if state.completed() {
                actual
                    .filter(|actual| {
                        actual.file_name() != path.as_deref().and_then(std::path::Path::file_name)
                    })
                    .map(|path| path.to_string_lossy().into_owned())
            } else if state == ItemState::Failed {
                Some(Error::from_hresult(result).message().trim().to_owned())
            } else if state == ItemState::Unconfirmed {
                Some(format!(
                    "Windows returned 0x{:08X} (resulting item {}).",
                    result.0 as u32,
                    if destination.as_ref().is_some() {
                        "present"
                    } else {
                        "absent"
                    }
                ))
            } else {
                None
            };
            tracker.record(index, state, detail);
        }
        // Interrupt the remaining Windows work after recording what just finished.
        canceled(&tracker.cancel)
    }
}

fn classify(action: Transfer, code: HRESULT, destination: bool) -> ItemState {
    if code == COPYENGINE_S_USER_IGNORED {
        ItemState::Skipped
    } else if is_cancel(code) {
        ItemState::Canceled
    } else if destination
        && (code == S_OK
            || (matches!(action, Transfer::Move) && code == COPYENGINE_S_DONT_PROCESS_CHILDREN))
    {
        match action {
            Transfer::Copy => ItemState::Copied,
            Transfer::Move => ItemState::Moved,
            Transfer::Extract => ItemState::Extracted,
            Transfer::Pack => ItemState::Packed,
        }
    } else if code.is_err() {
        ItemState::Failed
    } else {
        ItemState::Unconfirmed
    }
}

fn is_cancel(code: HRESULT) -> bool {
    matches!(code, COPYENGINE_E_USER_CANCELLED | COPYENGINE_E_CANCELLED)
        || code == ERROR_CANCELLED.to_hresult()
}

fn canceled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::from_hresult(ERROR_CANCELLED.to_hresult()))
    } else {
        Ok(())
    }
}

fn shell_path(value: Option<&IShellItem>) -> Option<PathBuf> {
    let result = unsafe {
        let text = value?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        if text.is_null() {
            return None;
        }
        let filename = OsString::from_wide(text.as_wide());
        CoTaskMemFree(Some(text.as_ptr().cast()));
        filename
    };
    Some(result.into())
}

#[allow(non_snake_case)]
impl IFileOperationProgressSink_Impl for Sink_Impl {
    fn StartOperations(&self) -> Result<()> {
        let mut tracker = self.0.lock().unwrap();
        canceled(&tracker.cancel)?;
        tracker.invoked = true;
        Ok(())
    }
    fn FinishOperations(&self, _: HRESULT) -> Result<()> {
        Ok(())
    }
    fn PreCopyItem(
        &self,
        _: u32,
        item: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
    ) -> Result<()> {
        self.before(item.as_ref())
    }
    fn PreMoveItem(
        &self,
        _: u32,
        item: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
    ) -> Result<()> {
        self.before(item.as_ref())?;
        let path = shell_path(item.as_ref());
        let mut tracker = self.0.lock().unwrap();
        if let Some(index) = path
            .as_ref()
            .and_then(|path| tracker.paths.get(path).copied())
            && tracker
                .undo_candidates
                .get(&index)
                .is_some_and(|candidate| !candidate.destination_empty())
        {
            tracker.undo_candidates.remove(&index);
        }
        Ok(())
    }
    fn PostCopyItem(
        &self,
        _: u32,
        item: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        result: HRESULT,
        dest: Ref<IShellItem>,
    ) -> Result<()> {
        self.after(item, result, dest)
    }
    fn PostMoveItem(
        &self,
        _: u32,
        item: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        result: HRESULT,
        dest: Ref<IShellItem>,
    ) -> Result<()> {
        self.after(item, result, dest)
    }
    fn UpdateProgress(&self, total: u32, done: u32) -> Result<()> {
        let mut tracker = self.0.lock().unwrap();
        tracker.progress.percent =
            (total > 0).then(|| (100 * u64::from(done.min(total)) / u64::from(total)) as u32);
        tracker.tick();
        canceled(&tracker.cancel)
    }
    fn PreRenameItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> Result<()> {
        self.check_cancel()
    }
    fn PostRenameItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> Result<()> {
        self.check_cancel()
    }
    fn PreDeleteItem(&self, _: u32, _: Ref<IShellItem>) -> Result<()> {
        self.check_cancel()
    }
    fn PostDeleteItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> Result<()> {
        self.check_cancel()
    }
    fn PreNewItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> Result<()> {
        self.check_cancel()
    }
    fn PostNewItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: &PCWSTR,
        _: u32,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> Result<()> {
        self.check_cancel()
    }
    fn ResetTimer(&self) -> Result<()> {
        Ok(())
    }
    fn PauseTimer(&self) -> Result<()> {
        Ok(())
    }
    fn ResumeTimer(&self) -> Result<()> {
        Ok(())
    }
}

pub fn run(
    request: &Request,
    owner: usize,
    cancel: Arc<AtomicBool>,
    send: impl Fn(ProgressUpdate) + Send + 'static,
) -> Report {
    if let Policy::Extract(source) = &request.policy {
        return run_extract(request, source, &cancel, send);
    }
    if let Policy::Pack(archive_name) = &request.policy {
        return run_pack(request, archive_name, &cancel, send);
    }
    let tracker = Arc::new(Mutex::new(Tracker {
        action: request.action,
        paths: HashMap::new(),
        undo_candidates: HashMap::new(),
        report: Report::empty(request.files.len()),
        cancel: cancel.clone(),
        send: Box::new(send),
        progress: Progress::default(),
        changes: Vec::new(),
        last: Instant::now(),
        invoked: false,
    }));
    let result = perform(request, owner, &cancel, &tracker);
    let mut tracker = tracker.lock().unwrap();
    let invoked = tracker.invoked;
    if let Err(error) = result {
        tracker.report.aborted |= is_cancel(error.code());
        tracker.report.failed |= !is_cancel(error.code());
        tracker.report.note = error.message().trim().to_owned();
    }
    let aborted = tracker.report.aborted;
    for row in &mut tracker.report.items {
        if row.state == ItemState::Pending {
            row.state = if invoked {
                ItemState::Unconfirmed
            } else if aborted {
                ItemState::Canceled
            } else {
                ItemState::NotStarted
            };
        }
        if !matches!(
            row.state,
            ItemState::Failed | ItemState::Skipped | ItemState::Canceled | ItemState::NotStarted
        ) {
            row.retry_identity = None;
        }
    }
    tracker.report.completed = tracker.progress.complete;
    tracker.report.retryable = tracker
        .report
        .items
        .iter()
        .filter(|item| item.retry_identity.is_some())
        .count();
    tracker.report.unconfirmed = tracker
        .report
        .items
        .iter()
        .any(|row| row.state == ItemState::Unconfirmed);
    std::mem::replace(&mut tracker.report, Report::empty(0))
}

fn run_extract(
    request: &Request,
    source: &archive::Source,
    cancel: &AtomicBool,
    send: impl Fn(ProgressUpdate),
) -> Report {
    let current = || !cancel.load(Ordering::Relaxed);
    let mut extractor = match archive::Extractor::prepare(
        &request.paths[0],
        source,
        &request.paths[1],
        request.files.iter().map(|file| file.name.as_os_str()),
        current,
    ) {
        Ok(extractor) => extractor,
        Err(error) => {
            let state = if current() {
                ItemState::NotStarted
            } else {
                ItemState::Canceled
            };
            return Report::stopped(request.files.len(), state, error.to_string());
        }
    };
    let mut report = Report::stopped(request.files.len(), ItemState::NotStarted, String::new());
    report.failed = false;
    let total = extractor.total;
    let mut bytes = 0_u128;
    let mut complete = 0;
    let started = Instant::now();
    let mut last = started;
    let mut changed = Vec::new();
    send(ProgressUpdate {
        summary: streaming_progress(complete, bytes, total, Duration::ZERO),
        items: Vec::new(),
    });
    for (index, file) in request.files.iter().enumerate() {
        if !current() {
            report.aborted = true;
            break;
        }
        let result = extractor.item(index, &file.name, current, |count| {
            bytes += count as u128;
            if last.elapsed() >= Duration::from_millis(100) {
                send(ProgressUpdate {
                    summary: streaming_progress(complete, bytes, total, started.elapsed()),
                    items: std::mem::take(&mut changed),
                });
                last = Instant::now();
            }
        });
        let (state, detail) = match result {
            Ok(()) => {
                complete += 1;
                (ItemState::Extracted, None)
            }
            Err(error) if !current() || error.kind() == io::ErrorKind::Interrupted => {
                report.aborted = true;
                (
                    ItemState::Canceled,
                    Some("Extraction stopped before publishing this item.".into()),
                )
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                (ItemState::Skipped, Some(error.to_string()))
            }
            Err(error) => {
                report.failed = true;
                (ItemState::Failed, Some(error.to_string()))
            }
        };
        report.items[index] = ItemReport {
            state,
            detail,
            retry_identity: None,
        };
        changed.push((index, state));
        if last.elapsed() >= Duration::from_millis(100) || index + 1 == request.files.len() {
            send(ProgressUpdate {
                summary: streaming_progress(complete, bytes, total, started.elapsed()),
                items: std::mem::take(&mut changed),
            });
            last = Instant::now();
        }
    }
    report.completed = complete;
    let skipped = report
        .items
        .iter()
        .filter(|item| item.state == ItemState::Skipped)
        .count();
    let failed = report
        .items
        .iter()
        .filter(|item| item.state == ItemState::Failed)
        .count();
    report.note = if report.aborted {
        format!(
            "Extraction canceled · {} extracted",
            grouped(complete as u64)
        )
    } else {
        format!(
            "{} extracted · {} skipped · {} failed",
            grouped(complete as u64),
            grouped(skipped as u64),
            grouped(failed as u64)
        )
    };
    report
}

fn run_pack(
    request: &Request,
    archive_name: &OsStr,
    cancel: &AtomicBool,
    send: impl Fn(ProgressUpdate),
) -> Report {
    let current = || !cancel.load(Ordering::Relaxed);
    let names: Vec<OsString> = request.files.iter().map(|file| file.name.clone()).collect();
    let mut packer = match archive::Packer::prepare(
        &request.paths[0],
        &request.paths[1],
        archive_name,
        &names,
        current,
    ) {
        Ok(packer) => packer,
        Err(error) => {
            let state = if current() {
                ItemState::NotStarted
            } else {
                ItemState::Canceled
            };
            return Report::stopped(request.files.len(), state, error.to_string());
        }
    };
    let mut report = Report::stopped(request.files.len(), ItemState::NotStarted, String::new());
    report.failed = false;
    let total = packer.total;
    let mut bytes = 0_u128;
    let mut complete = 0;
    let started = Instant::now();
    let mut last = started;
    let mut changed = Vec::new();
    send(ProgressUpdate {
        summary: streaming_progress(complete, bytes, total, Duration::ZERO),
        items: Vec::new(),
    });
    for index in 0..request.files.len() {
        if !current() {
            report.aborted = true;
            break;
        }
        let result = packer.item(index, current, |count| {
            bytes += count as u128;
            if last.elapsed() >= Duration::from_millis(100) {
                send(ProgressUpdate {
                    summary: streaming_progress(complete, bytes, total, started.elapsed()),
                    items: std::mem::take(&mut changed),
                });
                last = Instant::now();
            }
        });
        let (state, detail) = match result {
            Ok(()) => {
                complete += 1;
                (ItemState::Packed, None)
            }
            Err(error) if !current() || error.kind() == io::ErrorKind::Interrupted => {
                report.aborted = true;
                (
                    ItemState::Canceled,
                    Some("Packing stopped before publishing the archive.".into()),
                )
            }
            Err(error) => {
                report.failed = true;
                (ItemState::Failed, Some(error.to_string()))
            }
        };
        report.items[index] = ItemReport {
            state,
            detail,
            retry_identity: None,
        };
        changed.push((index, state));
        if last.elapsed() >= Duration::from_millis(100) || index + 1 == request.files.len() {
            send(ProgressUpdate {
                summary: streaming_progress(complete, bytes, total, started.elapsed()),
                items: std::mem::take(&mut changed),
            });
            last = Instant::now();
        }
        if report.aborted || report.failed {
            break;
        }
    }
    if !report.aborted && !report.failed {
        match packer.finish(current) {
            Ok(()) => {
                report.completed = complete;
                report.note = format!(
                    "Created {} · {} packed",
                    archive_name.to_string_lossy(),
                    grouped(complete as u64)
                );
            }
            Err(error) => {
                let canceled_now = !current() || error.kind() == io::ErrorKind::Interrupted;
                report.aborted = canceled_now;
                report.failed = !canceled_now;
                for item in &mut report.items {
                    if item.state == ItemState::Packed {
                        item.state = if canceled_now {
                            ItemState::Canceled
                        } else {
                            ItemState::Failed
                        };
                        item.detail = Some(error.to_string());
                    }
                }
                report.completed = 0;
                report.note = error.to_string();
            }
        }
    } else {
        for item in &mut report.items {
            if item.state == ItemState::Packed {
                item.state = if report.aborted {
                    ItemState::Canceled
                } else {
                    ItemState::NotStarted
                };
            }
        }
        report.completed = 0;
        report.note = if report.aborted {
            "Packing canceled · no archive was created".into()
        } else {
            "Packing failed · no archive was created".into()
        };
    }
    report
}

fn io_error(error: io::Error) -> Error {
    if error.kind() == io::ErrorKind::Interrupted {
        Error::from_hresult(ERROR_CANCELLED.to_hresult())
    } else {
        Error::new(E_FAIL, error.to_string())
    }
}

fn perform(
    request: &Request,
    owner: usize,
    cancel: &AtomicBool,
    tracker: &Arc<Mutex<Tracker>>,
) -> Result<()> {
    canceled(cancel)?;
    let roots = if request.incoming_transfer() {
        None
    } else {
        Some(request.identities().map_err(io_error)?)
    };
    let destination = if let Some(roots) = roots {
        roots[1]
    } else {
        compare::directory_identity(&request.paths[1]).map_err(io_error)?
    };
    tracker.lock().unwrap().report.roots = roots;
    if matches!(request.policy, Policy::Reviewed(_) | Policy::Retry(_)) {
        compare::verify_review(
            &request.paths,
            request.files.iter().map(|file| {
                (
                    file.name.as_os_str(),
                    file.target(),
                    file.stamp.unwrap(),
                    file.replacement,
                )
            }),
            || !cancel.load(Ordering::Relaxed),
        )
        .map_err(io_error)?;
    }

    let _apartment = shell::Apartment::enter()?;
    let operation: IFileOperation =
        unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)? };
    let target_folder = shell::item(&request.paths[1])?;
    unsafe {
        operation.SetOwnerWindow(HWND(owner as _))?;
        operation.SetOperationFlags(FOF_NOCONFIRMMKDIR | FOFX_ADDUNDORECORD)?;
    }
    let mut registered = 0;
    for (index, file) in request.files.iter().enumerate() {
        canceled(cancel)?;
        match register(
            request,
            index,
            file,
            &operation,
            &target_folder,
            tracker,
            destination,
        ) {
            Ok(()) => registered += 1,
            Err(error) => {
                let mut state = tracker.lock().unwrap();
                state.report.failed = true;
                state.record(
                    index,
                    ItemState::Failed,
                    Some(error.message().trim().to_owned()),
                );
                if !request.incoming_transfer() {
                    return Err(error);
                }
            }
        }
    }
    if registered == 0 {
        return Err(Error::new(
            E_FAIL,
            "None of the supplied files could be transferred.",
        ));
    }
    let same_folders = if let Some(roots) = roots {
        request.identities().map_err(io_error)? == roots
    } else {
        compare::directory_identity(&request.paths[1]).map_err(io_error)? == destination
    };
    if !same_folders {
        return Err(Error::new(
            E_FAIL,
            "A folder changed while preparing the transfer. Review the folders again.",
        ));
    }

    let sink: IFileOperationProgressSink = Sink(tracker.clone()).into();
    let cookie = unsafe { operation.Advise(&sink)? };
    let result = unsafe { operation.PerformOperations() };
    let aborted = unsafe { operation.GetAnyOperationsAborted() };
    unsafe {
        let _ = operation.Unadvise(cookie);
    }
    if let Ok(aborted) = aborted {
        tracker.lock().unwrap().report.aborted |= aborted.as_bool();
    }
    result?;
    let _ = aborted?;
    Ok(())
}

fn register(
    request: &Request,
    index: usize,
    file: &Requested,
    operation: &IFileOperation,
    folder: &IShellItem,
    tracker: &Arc<Mutex<Tracker>>,
    destination_folder: Identity,
) -> Result<()> {
    let source = request.source_path(file);
    let target = request.paths[1].join(file.target());
    let stamp = compare::path_stamp(&source).map_err(io_error)?;
    if stamp.is_none() || file.stamp.is_some_and(|expected| Some(expected) != stamp) {
        return Err(Error::new(
            E_FAIL,
            format!(
                "The source changed or is missing. Select it again.\n{}",
                source.display()
            ),
        ));
    }
    if request.incoming_transfer()
        && compare::directory_identity(source.parent().unwrap()).map_err(io_error)?
            == destination_folder
    {
        return Err(Error::new(
            E_FAIL,
            "This item is already in the destination folder.",
        ));
    }
    let destination = compare::path_stamp(&target).map_err(io_error)?;
    if matches!(request.policy, Policy::Reviewed(_) | Policy::Retry(_))
        && destination != file.replacement
    {
        return Err(Error::new(
            E_FAIL,
            format!(
                "The destination changed. Review it before copying or moving.\n{}",
                target.display()
            ),
        ));
    }
    let identity = if !request.incoming_transfer()
        && destination.is_none()
        && stamp.is_some_and(Stamp::regular)
    {
        compare::regular_identity(&source)
            .ok()
            .and_then(|(identity, observed)| (Some(observed) == stamp).then_some(identity))
    } else {
        None
    };
    if file.identity.is_some() && identity != file.identity {
        return Err(Error::new(
            E_FAIL,
            format!(
                "The source is no longer the verified file. Review it again.\n{}",
                source.display()
            ),
        ));
    }
    let shell_item = shell::item(&source)?;
    let canonical = shell_path(Some(&shell_item)).unwrap_or_else(|| source.clone());
    {
        let mut state = tracker.lock().unwrap();
        if state.paths.contains_key(&canonical) {
            return Err(Error::new(
                E_FAIL,
                "Two selected names refer to the same Windows Shell item.",
            ));
        }
        state.paths.insert(canonical, index);
        state.report.items[index].retry_identity = identity;
        if matches!(request.action, Transfer::Move)
            && destination.is_none()
            && stamp.is_some_and(Stamp::regular)
            && state.undo_candidates.len() < undo::MAX_ITEMS
            && let Some(candidate) = undo::Candidate::capture(&source, &target)
        {
            state.undo_candidates.insert(index, candidate);
        }
    }
    let name = file.target.as_ref().map(wide);
    let name = name
        .as_ref()
        .map_or(PCWSTR::null(), |name| PCWSTR(name.as_ptr()));
    unsafe {
        match request.action {
            Transfer::Copy => operation.CopyItem(&shell_item, folder, name, None),
            Transfer::Move => operation.MoveItem(&shell_item, folder, name, None),
            Transfer::Extract | Transfer::Pack => {
                unreachable!("ZIP transfers use the archive worker")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_shell_move_completion_from_skip_and_unknown() {
        assert!(matches!(
            classify(Transfer::Move, COPYENGINE_S_DONT_PROCESS_CHILDREN, true),
            ItemState::Moved
        ));
        assert!(matches!(
            classify(Transfer::Move, COPYENGINE_S_DONT_PROCESS_CHILDREN, false),
            ItemState::Unconfirmed
        ));
        assert!(matches!(
            classify(Transfer::Copy, COPYENGINE_S_USER_IGNORED, true),
            ItemState::Skipped
        ));
        assert!(matches!(
            classify(Transfer::Copy, COPYENGINE_E_USER_CANCELLED, false),
            ItemState::Canceled
        ));
        assert!(matches!(
            classify(Transfer::Copy, S_OK, true),
            ItemState::Copied
        ));
    }
}
