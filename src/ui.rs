use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    ffi::{OsString, c_void},
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

use windows::{
    Win32::{
        Foundation::*,
        Globalization::{CSTR_EQUAL, CompareStringOrdinal},
        Graphics::{Dwm::*, Gdi::*},
        System::{
            Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
            LibraryLoader::GetModuleHandleW,
            Registry::*,
        },
        UI::{
            Accessibility::{
                CLSID_AccPropServices, IAccPropServices, Name_Property_GUID, PROPID_ACC_NAME,
            },
            Controls::*,
            HiDpi::*,
            Input::KeyboardAndMouse::*,
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
    core::{PCWSTR, PWSTR, Result, w},
};

use crate::{
    directory::{Column, Entry, InitialScan, Rows, Scanner, Sort, Update, grouped, wide},
    shell::{self, Icons, Operation, Transfer},
    watch::{Notice, Rename, Watcher},
};

mod paint;
mod prompt;
mod workspace;

use workspace::Workspace;

const WAKE: u32 = WM_APP + 1;
const COPY: u16 = 101;
const MOVE: u16 = 102;
const REFRESH: u16 = 103;
const VIEW: u16 = 104;
const RENAME: u16 = 105;
const CREATE_FOLDER: u16 = 106;
const RECYCLE: u16 = 107;
const SORT_NAME: u16 = 114;
const SORT_SIZE: u16 = 115;
const SORT_MODIFIED: u16 = 116;
const SYSTEM: u16 = 110;
const LIGHT: u16 = 111;
const DARK: u16 = 112;
const LEFT_UP: u16 = 121;
const RIGHT_UP: u16 = 122;
const LEFT_CLEAR: u16 = 125;
const RIGHT_CLEAR: u16 = 126;
const LEFT_FILTER: u16 = 151;
const RIGHT_FILTER: u16 = 152;
const SCAN_TIMER: usize = 200;
const REFRESH_TIMER: usize = 210;

enum Event {
    Directory(Update),
    Watch(usize, usize, Notice),
    Operation(&'static str, Changes, Result<bool>),
}

struct Changes {
    folders: Vec<PathBuf>,
    rename: Option<Rename>,
    reveal: Option<(usize, usize, usize, Vec<u16>)>,
}

impl Changes {
    fn folder(folder: PathBuf) -> Self {
        Self {
            folders: vec![folder],
            rename: None,
            reveal: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum WatchState {
    #[default]
    Pending,
    Active,
    Unavailable,
}

#[derive(Default)]
struct Listing {
    path: PathBuf,
    location: usize,
    request: usize,
    view_request: usize,
    entries: Arc<Vec<Entry>>,
    rows: Rows,
    sort: Sort,
    query: Vec<u16>,
    loading: bool,
    complete: bool,
    projecting: bool,
    keep_viewport: bool,
    error: Option<String>,
    watch: WatchState,
    initial: Option<Arc<InitialScan>>,
    refresh_pending: bool,
    dirty: bool,
    pending_renames: Vec<Rename>,
    scan_renames: Vec<Rename>,
    display_renames: Vec<Rename>,
    selection_epoch: usize,
    pending_reveal: Option<(usize, Vec<u16>)>,
    reveal: Option<(usize, Vec<u16>)>,
}

impl Listing {
    fn ready(&self) -> bool {
        self.complete && !self.projecting && self.error.is_none()
    }
}

struct Pane {
    path: HWND,
    up: HWND,
    filter: HWND,
    clear: HWND,
    list: HWND,
    scanner: Scanner,
    watcher: Watcher,
    listing: RefCell<Listing>,
}

#[derive(Clone, Copy)]
enum Theme {
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Default)]
struct Layout {
    panes: [RECT; 2],
    gutter: RECT,
    footer: RECT,
}

struct App {
    window: HWND,
    panes: [Pane; 2],
    copy: HWND,
    move_: HWND,
    rename: HWND,
    create: HWND,
    recycle: HWND,
    view: HWND,
    menu: HMENU,
    accessibility: Option<IAccPropServices>,
    active: Cell<usize>,
    dpi: Cell<u32>,
    font: Cell<HFONT>,
    theme: Cell<Theme>,
    palette: RefCell<paint::Palette>,
    layout: Cell<Layout>,
    split: Cell<f32>,
    dragging: Cell<bool>,
    changing_rows: Cell<bool>,
    icons: Option<Icons>,
    operation_active: Cell<bool>,
    operation_status: RefCell<String>,
    events: Receiver<Event>,
    sender: Sender<Event>,
    persist: bool,
}

impl Drop for App {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.font.get().into());
            let _ = DestroyMenu(self.menu);
        }
    }
}

pub fn startup_error(error: &str) {
    message(
        HWND::default(),
        "Dual Pane could not start",
        error,
        MB_OK | MB_ICONERROR,
    );
}

fn message(owner: HWND, title: &str, text: &str, style: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    let title = wide(title);
    let text = wide(text);
    unsafe {
        MessageBoxW(
            Some(owner),
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            style,
        )
    }
}

pub fn run() -> Result<()> {
    let _apartment = shell::Apartment::enter()?;
    let workspace = Workspace::startup();
    unsafe {
        let window = create_window()?;
        let app = attach(window, &workspace)?;
        let [left, right] = workspace.paths;
        app.navigate(0, left);
        app.navigate(1, right);
        let _ = ShowWindow(window, SW_SHOW);
        app.focus_list(0);

        let mut msg = MSG::default();
        loop {
            match GetMessageW(&mut msg, None, 0, 0).0 {
                -1 => return Err(windows::core::Error::from_thread()),
                0 => break,
                _ => {
                    if msg.message == WM_SYSKEYDOWN
                        && msg.wParam.0 == VK_V.0 as usize
                        && msg.lParam.0 & (1 << 29) != 0
                    {
                        app.view_menu();
                        continue;
                    }
                    if msg.message == WM_KEYDOWN && app.key(msg.hwnd, msg.wParam.0 as u16) {
                        continue;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
    Ok(())
}

fn create_window() -> Result<HWND> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = HINSTANCE(GetModuleHandleW(None)?.0);
        let controls = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_LISTVIEW_CLASSES | ICC_STANDARD_CLASSES,
        };
        InitCommonControlsEx(&controls).ok()?;
        let class = w!("DualPaneWindow");
        let wc = WNDCLASSW {
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hIcon: LoadIconW(None, IDI_APPLICATION)?,
            hInstance: instance,
            lpfnWndProc: Some(window_proc),
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        let dpi = GetDpiForSystem().max(96) as i32;
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("Dual Pane"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1130 * dpi / 96,
            700 * dpi / 96,
            None,
            None,
            Some(instance),
            None,
        )
    }
}

fn attach(window: HWND, workspace: &Workspace) -> Result<Box<App>> {
    let app = Box::new(App::new(window, workspace)?);
    unsafe {
        SetWindowLongPtrW(window, GWLP_USERDATA, (&*app as *const App) as isize);
    }
    app.update_font();
    app.apply_theme();
    app.layout();
    app.update_sort_header(0);
    app.update_sort_header(1);
    Ok(app)
}
fn child(window: HWND, class: PCWSTR, label: PCWSTR, style: WINDOW_STYLE, id: u16) -> Result<HWND> {
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            label,
            WS_CHILD | WS_VISIBLE | style,
            0,
            0,
            0,
            0,
            Some(window),
            Some(HMENU(id as usize as *mut c_void)),
            None,
            None,
        )
    }
}

fn button(window: HWND, label: PCWSTR, id: u16) -> Result<HWND> {
    child(
        window,
        w!("Button"),
        label,
        WINDOW_STYLE(BS_OWNERDRAW as u32),
        id,
    )
}

fn menu_label(menu: HMENU, id: u16, label: PCWSTR) -> Result<()> {
    unsafe { AppendMenuW(menu, MF_STRING, id as usize, label) }
}

fn name_accessible_controls(panes: &[Pane; 2]) -> Option<IAccPropServices> {
    let service: IAccPropServices =
        unsafe { CoCreateInstance(&CLSID_AccPropServices, None, CLSCTX_INPROC_SERVER) }.ok()?;
    for (control, name) in [
        (panes[0].path, w!("Left folder path")),
        (panes[0].filter, w!("Filter left folder by filename")),
        (panes[0].list, w!("Left folder contents")),
        (panes[1].path, w!("Right folder path")),
        (panes[1].filter, w!("Filter right folder by filename")),
        (panes[1].list, w!("Right folder contents")),
    ] {
        for property in [Name_Property_GUID, PROPID_ACC_NAME] {
            unsafe {
                let _ = service.SetHwndPropStr(
                    control,
                    OBJID_CLIENT.0 as u32,
                    CHILDID_SELF,
                    property,
                    name,
                );
            }
        }
    }
    Some(service)
}

impl Pane {
    fn new(
        window: HWND,
        index: usize,
        sender: Sender<Event>,
        icons: Option<&Icons>,
        sort: Sort,
    ) -> Result<Self> {
        let path = child(
            window,
            w!("Edit"),
            PCWSTR::null(),
            WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            131 + index as u16,
        )?;
        let up = button(window, w!("Up"), LEFT_UP + index as u16)?;
        let filter = child(
            window,
            w!("Edit"),
            PCWSTR::null(),
            WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            LEFT_FILTER + index as u16,
        )?;
        let clear = button(window, w!("Clear"), LEFT_CLEAR + index as u16)?;
        send(filter, EM_SETLIMITTEXT, 255, 0);
        let list = child(
            window,
            WC_LISTVIEWW,
            PCWSTR::null(),
            WS_TABSTOP
                | WINDOW_STYLE(
                    LVS_REPORT | LVS_OWNERDATA | LVS_SHOWSELALWAYS | LVS_SHAREIMAGELISTS,
                ),
            141 + index as u16,
        )?;
        send(path, EM_SETLIMITTEXT, 32767, 0);
        send(list, CCM_SETUNICODEFORMAT, 1, 0);
        send(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            0,
            (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP) as isize,
        );
        if let Some(icons) = icons {
            send(
                list,
                LVM_SETIMAGELIST,
                LVSIL_SMALL as usize,
                icons.list as isize,
            );
        }
        for (index, (label, format)) in [
            (w!("Name"), LVCFMT_LEFT),
            (w!("Size"), LVCFMT_RIGHT),
            (w!("Modified"), LVCFMT_LEFT),
        ]
        .into_iter()
        .enumerate()
        {
            let mut column = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_FMT | LVCF_WIDTH,
                fmt: format,
                cx: 60,
                pszText: PWSTR(label.as_ptr().cast_mut()),
                ..Default::default()
            };
            send(
                list,
                LVM_INSERTCOLUMNW,
                index,
                (&mut column as *mut LVCOLUMNW) as isize,
            );
        }
        let owner = window.0 as usize;
        let scan_sender = sender.clone();
        let scanner = Scanner::start(index, move |update| {
            if scan_sender.send(Event::Directory(update)).is_ok() {
                wake(owner);
            }
        })
        .map_err(|error| windows::core::Error::new(E_FAIL, error.to_string()))?;
        let watcher = Watcher::start(index, move |id, notice| {
            if sender.send(Event::Watch(index, id, notice)).is_ok() {
                wake(owner);
            }
        })?;
        Ok(Self {
            path,
            up,
            filter,
            clear,
            list,
            scanner,
            watcher,
            listing: RefCell::new(Listing {
                sort,
                ..Listing::default()
            }),
        })
    }

    fn count(&self) -> usize {
        send(self.list, LVM_GETSELECTEDCOUNT, 0, 0) as usize
    }

    fn item(&self, index: isize) -> Option<(PathBuf, bool)> {
        let listing = self.listing.borrow();
        let row = listing.rows.get(index as usize)?;
        Some((listing.path.join(row.filename()), row.directory))
    }

    fn selected_paths(&self) -> (PathBuf, Vec<PathBuf>) {
        let listing = self.listing.borrow();
        let paths = self
            .selection()
            .filter_map(|row| listing.rows.get(row))
            .map(|row| listing.path.join(row.filename()))
            .collect();
        (listing.path.clone(), paths)
    }

    fn selection(&self) -> impl Iterator<Item = usize> + '_ {
        let mut previous = -1;
        std::iter::from_fn(move || {
            previous = send(
                self.list,
                LVM_GETNEXTITEM,
                previous as usize,
                LVNI_SELECTED as isize,
            );
            usize::try_from(previous).ok()
        })
    }
}

impl App {
    fn new(window: HWND, workspace: &Workspace) -> Result<Self> {
        let (sender, events) = mpsc::channel();
        let icons = Icons::load();
        let panes = [
            Pane::new(window, 0, sender.clone(), icons.as_ref(), workspace.sort[0])?,
            Pane::new(window, 1, sender.clone(), icons.as_ref(), workspace.sort[1])?,
        ];
        let copy = button(window, w!("F5   Copy"), COPY)?;
        let move_ = button(window, w!("F6   Move"), MOVE)?;
        let rename = button(window, w!("F2   Rename"), RENAME)?;
        let create = button(window, w!("F7   Folder"), CREATE_FOLDER)?;
        let recycle = button(window, w!("Del   Recycle"), RECYCLE)?;
        let view = button(window, w!("View   ▾"), VIEW)?;
        let menu = unsafe {
            let menu = CreatePopupMenu()?;
            menu_label(menu, REFRESH, w!("&Refresh active pane\tCtrl+R"))?;
            AppendMenuW(menu, MF_SEPARATOR, 0, None)?;
            menu_label(menu, SORT_NAME, w!("Sort by &name\tCtrl+1"))?;
            menu_label(menu, SORT_SIZE, w!("Sort by si&ze\tCtrl+2"))?;
            menu_label(menu, SORT_MODIFIED, w!("Sort by &modified\tCtrl+3"))?;
            AppendMenuW(menu, MF_SEPARATOR, 0, None)?;
            menu_label(menu, SYSTEM, w!("&System theme"))?;
            menu_label(menu, LIGHT, w!("&Light theme"))?;
            menu_label(menu, DARK, w!("&Dark theme"))?;
            menu
        };
        let accessibility = name_accessible_controls(&panes);
        let app = Self {
            window,
            panes,
            copy,
            move_,
            rename,
            create,
            recycle,
            view,
            menu,
            accessibility,
            active: Cell::new(0),
            dpi: Cell::new(unsafe { GetDpiForWindow(window) }.max(96)),
            font: Cell::new(HFONT::default()),
            theme: Cell::new(workspace.theme),
            palette: RefCell::new(paint::Palette::new(workspace.theme)),
            layout: Cell::new(Layout::default()),
            split: Cell::new(workspace.split),
            dragging: Cell::new(false),
            changing_rows: Cell::new(false),
            icons,
            operation_active: Cell::new(false),
            operation_status: RefCell::new(String::new()),
            events,
            sender,
            persist: workspace.persist,
        };
        for pane in &app.panes {
            unsafe {
                SetWindowSubclass(pane.list, Some(control_proc), 1, window.0 as usize).ok()?;
            }
        }
        Ok(app)
    }

    fn status_changed(&self) {
        let layout = self.layout.get();
        unsafe {
            for pane in layout.panes {
                let rect = RECT {
                    top: pane.bottom - self.scale(29),
                    ..pane
                };
                let _ = InvalidateRect(Some(self.window), Some(&rect), false);
            }
            let _ = InvalidateRect(Some(self.window), Some(&layout.footer), false);
        }
        let source = self.active.get();
        let destination = self.panes[1 - source].listing.borrow();
        let origin = self.panes[source].listing.borrow();
        let ready = !self.operation_active.get() && origin.ready();
        let selected = self.panes[source].count();
        let enabled = ready
            && destination.complete
            && destination.error.is_none()
            && origin.path != destination.path
            && selected > 0;
        drop(origin);
        drop(destination);
        unsafe {
            let _ = EnableWindow(self.copy, enabled);
            let _ = EnableWindow(self.move_, enabled);
            let _ = EnableWindow(self.rename, ready && selected == 1);
            let _ = EnableWindow(self.create, ready);
            let _ = EnableWindow(self.recycle, ready && selected > 0);
        }
        for pane in &self.panes {
            let clear = !pane.listing.borrow().query.is_empty();
            unsafe {
                let _ = EnableWindow(pane.clear, clear);
            }
        }
    }

    fn active(&self, index: usize) {
        if self.active.replace(index) != index {
            unsafe {
                let _ = InvalidateRect(Some(self.window), None, false);
            }
            self.status_changed();
        }
    }

    fn focus_list(&self, index: usize) {
        self.active(index);
        unsafe {
            let _ = SetFocus(Some(self.panes[index].list));
        }
    }

    fn navigate(&self, index: usize, path: PathBuf) {
        let pane = &self.panes[index];
        let path = match std::path::absolute(path) {
            Ok(path) => path,
            Err(error) => {
                message(
                    self.window,
                    "Invalid path",
                    &error.to_string(),
                    MB_OK | MB_ICONWARNING,
                );
                return;
            }
        };
        self.kill_timer(SCAN_TIMER + index);
        self.kill_timer(REFRESH_TIMER + index);
        let (location, initial) = {
            let mut listing = pane.listing.borrow_mut();
            let request = listing.request.wrapping_add(1);
            let initial = pane.scanner.prepare_initial(request, path.clone());
            *listing = Listing {
                path: path.clone(),
                location: listing.location.wrapping_add(1),
                request,
                view_request: listing.view_request.wrapping_add(1),
                sort: listing.sort,
                loading: true,
                initial: Some(initial.clone()),
                ..Listing::default()
            };
            pane.scanner.cancel_view(listing.view_request);
            (listing.location, initial)
        };
        self.changing_rows.set(true);
        send(pane.list, LVM_SETITEMCOUNT, 0, 0);
        self.changing_rows.set(false);
        show_path(pane.path, &path);
        unsafe {
            let _ = SetWindowTextW(pane.filter, w!(""));
            let _ = EnableWindow(pane.up, parent_of(&path).is_some());
        }
        pane.watcher.watch(location, path, Some(initial));
        // Give the watch a chance to arm before enumerating. A slow or unsupported
        // watch never gets to hold up directory browsing.
        if !self.timer(SCAN_TIMER + index, 75) {
            self.start_initial_scan(index);
        }
        self.status_changed();
    }

    fn timer(&self, id: usize, milliseconds: u32) -> bool {
        unsafe { SetTimer(Some(self.window), id, milliseconds, None) != 0 }
    }

    fn kill_timer(&self, id: usize) {
        unsafe {
            let _ = KillTimer(Some(self.window), id);
        }
    }

    fn start_initial_scan(&self, index: usize) {
        let pane = &self.panes[index];
        self.kill_timer(SCAN_TIMER + index);
        if let Some(initial) = pane.listing.borrow_mut().initial.take() {
            initial.start();
        }
    }

    fn queue_refresh(&self, index: usize) {
        let pane = &self.panes[index];
        let mut listing = pane.listing.borrow_mut();
        listing.dirty = true;
        if listing.loading || listing.refresh_pending {
            return;
        }
        listing.refresh_pending = self.timer(REFRESH_TIMER + index, 100);
        let queued = listing.refresh_pending;
        drop(listing);
        if !queued {
            self.scan_dirty_folder(index);
        }
    }

    fn scan_dirty_folder(&self, index: usize) {
        self.kill_timer(REFRESH_TIMER + index);
        let mut listing = self.panes[index].listing.borrow_mut();
        listing.refresh_pending = false;
        let scan = listing.dirty && !listing.loading;
        drop(listing);
        if scan {
            self.scan_folder(index);
        }
    }

    fn scan_folder(&self, index: usize) {
        let pane = &self.panes[index];
        self.kill_timer(REFRESH_TIMER + index);
        let mut listing = pane.listing.borrow_mut();
        listing.request = listing.request.wrapping_add(1);
        listing.loading = true;
        listing.dirty = false;
        listing.refresh_pending = false;
        let pending = std::mem::take(&mut listing.pending_renames);
        add_renames(&mut listing.scan_renames, pending);
        pane.scanner.refresh(listing.request, listing.path.clone());
        drop(listing);
        self.status_changed();
    }

    fn receive_watch(&self, index: usize, location: usize, notice: Notice) {
        let mut listing = self.panes[index].listing.borrow_mut();
        if listing.location != location {
            return;
        }
        let initial = listing.initial.is_some();
        let refresh = match notice {
            Notice::Armed { reconcile } => {
                listing.watch = WatchState::Active;
                reconcile
            }
            Notice::Changed(renames) => {
                add_renames(&mut listing.pending_renames, renames);
                true
            }
            Notice::Unavailable => {
                let was_active = listing.watch == WatchState::Active;
                listing.watch = WatchState::Unavailable;
                was_active
            }
        };
        drop(listing);
        if initial {
            self.start_initial_scan(index);
        }
        if refresh {
            self.queue_refresh(index);
        }
        self.status_changed();
    }
    fn parent(&self, index: usize) {
        let parent = parent_of(&self.panes[index].listing.borrow().path);
        if let Some(parent) = parent {
            self.navigate(index, parent);
        }
        self.focus_list(index);
    }

    fn refresh(&self, index: usize) {
        let pane = &self.panes[index];
        let mut listing = pane.listing.borrow_mut();
        if listing.initial.is_some() {
            drop(listing);
            self.start_initial_scan(index);
            return;
        }
        if listing.watch == WatchState::Unavailable {
            listing.watch = WatchState::Pending;
            pane.watcher
                .watch(listing.location, listing.path.clone(), None);
        }
        drop(listing);
        self.scan_folder(index);
    }
    fn open_row(&self, index: usize, row: isize) {
        let pane = &self.panes[index];
        if pane.listing.borrow().projecting {
            return;
        }
        if let Some((path, directory)) = pane.item(row) {
            if directory {
                self.navigate(index, path);
            } else if let Err(error) = shell::open(self.window, &path) {
                message(
                    self.window,
                    "Could not open file",
                    &error.message(),
                    MB_OK | MB_ICONWARNING,
                );
            }
        }
    }

    fn receive_directory(
        &self,
        index: usize,
        id: usize,
        preview: bool,
        entries: Result<Vec<Entry>>,
    ) {
        let mut listing = self.panes[index].listing.borrow_mut();
        if listing.request != id {
            return;
        }
        listing.loading = preview;
        let changed = match entries {
            Ok(entries) => {
                listing.entries = Arc::new(entries);
                listing.complete = !preview;
                listing.error = None;
                if !preview {
                    let aliases = std::mem::take(&mut listing.scan_renames);
                    add_renames(&mut listing.display_renames, aliases);
                    if let Some(reveal) = listing.pending_reveal.take() {
                        listing.reveal = Some(reveal);
                    }
                }
                true
            }
            Err(error) => {
                listing.error = Some(error.message().trim().to_owned());
                false
            }
        };
        let dirty = listing.dirty;
        drop(listing);
        if changed {
            self.request_view(index, true);
        }
        if dirty && !preview {
            self.queue_refresh(index);
        }
        self.status_changed();
    }

    fn filter_changed(&self, index: usize) {
        let pane = &self.panes[index];
        let mut query = wide(window_text(pane.filter));
        query.pop();
        let mut listing = pane.listing.borrow_mut();
        if listing.query == query {
            return;
        }
        listing.query = query;
        drop(listing);
        self.request_view(index, false);
    }

    fn clear_filter(&self, index: usize) {
        unsafe {
            let _ = SetWindowTextW(self.panes[index].filter, w!(""));
        }
        self.focus_list(index);
    }

    fn sort_by(&self, index: usize, column: Column) {
        let pane = &self.panes[index];
        let mut listing = pane.listing.borrow_mut();
        let descending = if listing.sort.column == column {
            !listing.sort.descending
        } else {
            column == Column::Modified
        };
        listing.sort = Sort { column, descending };
        drop(listing);
        self.update_sort_header(index);
        self.request_view(index, false);
    }

    fn update_sort_header(&self, index: usize) {
        let pane = &self.panes[index];
        let sort = pane.listing.borrow().sort;
        let header = HWND(send(pane.list, LVM_GETHEADER, 0, 0) as _);
        for column in 0..3 {
            let mut item = HDITEMW {
                mask: HDI_FORMAT,
                ..Default::default()
            };
            if send(
                header,
                HDM_GETITEMW,
                column,
                (&mut item as *mut HDITEMW) as isize,
            ) != 0
            {
                item.fmt.0 &= !(HDF_SORTUP.0 | HDF_SORTDOWN.0);
                if sort.column.index() == column {
                    item.fmt.0 |= if sort.descending {
                        HDF_SORTDOWN.0
                    } else {
                        HDF_SORTUP.0
                    };
                }
                send(
                    header,
                    HDM_SETITEMW,
                    column,
                    (&item as *const HDITEMW) as isize,
                );
            }
        }
        unsafe {
            let _ = InvalidateRect(Some(header), None, false);
        }
    }

    fn request_view(&self, index: usize, keep_viewport: bool) {
        let pane = &self.panes[index];
        let mut listing = pane.listing.borrow_mut();
        listing.view_request = listing.view_request.wrapping_add(1);
        let id = listing.view_request;
        let entries = listing.entries.clone();
        let sort = listing.sort;
        let query = listing.query.clone();
        listing.projecting = !entries.is_empty() && (sort != Sort::default() || !query.is_empty());
        listing.keep_viewport = keep_viewport;
        let projecting = listing.projecting;
        drop(listing);
        if projecting {
            pane.scanner.project(id, entries, sort, query);
            self.status_changed();
        } else {
            pane.scanner.cancel_view(id);
            self.apply_rows(index, Rows::all(entries));
        }
    }

    fn apply_rows(&self, index: usize, rows: Rows) {
        let pane = &self.panes[index];
        let native_focus = send(
            pane.list,
            LVM_GETNEXTITEM,
            usize::MAX,
            LVNI_FOCUSED as isize,
        );
        let native_top = send(pane.list, LVM_GETTOPINDEX, 0, 0);
        let listing = pane.listing.borrow();
        let name_at = |row: isize| {
            listing
                .rows
                .get(row as usize)
                .map(|row| renamed(&row.name, &listing.display_renames))
        };
        let selected = pane
            .selection()
            .filter_map(|row| name_at(row as isize))
            .collect::<HashSet<_>>();
        let focused = if listing.keep_viewport || !selected.is_empty() {
            name_at(native_focus)
        } else {
            None
        };
        let top = if listing.keep_viewport {
            name_at(native_top)
        } else {
            None
        };
        let reveal = listing
            .reveal
            .as_ref()
            .filter(|(epoch, _)| *epoch == listing.selection_epoch)
            .map(|(_, name)| name.as_slice());
        let total = rows.len();
        let keep_viewport = listing.keep_viewport;
        let mut selected_rows = Vec::new();
        let mut focused_row = None;
        let mut top_row = None;
        let mut reveal_row = None;
        if !selected.is_empty() || focused.is_some() || top.is_some() || reveal.is_some() {
            for (row, entry) in rows.iter().enumerate() {
                if selected.contains(entry.name.as_ref()) {
                    selected_rows.push(row);
                }
                if focused == Some(entry.name.as_ref()) {
                    focused_row = Some(row);
                }
                if top == Some(entry.name.as_ref()) {
                    top_row = Some(row);
                }
                if reveal == Some(entry.name.as_ref()) {
                    reveal_row = Some(row);
                }
            }
        }
        drop(listing);
        {
            let mut listing = pane.listing.borrow_mut();
            listing.rows = rows;
            listing.projecting = false;
            listing.display_renames.clear();
            listing.reveal = None;
        }
        self.changing_rows.set(true);
        item_state(
            pane.list,
            usize::MAX,
            LIST_VIEW_ITEM_STATE_FLAGS(0),
            LVIS_SELECTED,
        );
        if native_focus >= 0 {
            item_state(
                pane.list,
                native_focus as usize,
                LIST_VIEW_ITEM_STATE_FLAGS(0),
                LVIS_FOCUSED,
            );
        }
        send(pane.list, LVM_SETITEMCOUNT, total, LVSICF_NOSCROLL as isize);
        if let Some(row) = reveal_row {
            item_state(pane.list, row, LVIS_SELECTED, LVIS_SELECTED);
        } else if total > 0 && selected_rows.len() == total {
            item_state(pane.list, usize::MAX, LVIS_SELECTED, LVIS_SELECTED);
        } else {
            for &row in &selected_rows {
                item_state(pane.list, row, LVIS_SELECTED, LVIS_SELECTED);
            }
        }
        if total > 0 {
            let focus = reveal_row
                .or(focused_row)
                .or_else(|| selected_rows.first().copied())
                .or(top_row)
                .unwrap_or(0);
            item_state(pane.list, focus, LVIS_FOCUSED, LVIS_FOCUSED);
            if reveal_row.is_some() {
                send(pane.list, LVM_ENSUREVISIBLE, focus, 0);
            } else if keep_viewport {
                let top = top_row.unwrap_or_else(|| (native_top.max(0) as usize).min(total - 1));
                scroll_to(pane.list, top);
            } else {
                scroll_to(pane.list, 0);
                send(pane.list, LVM_ENSUREVISIBLE, focus, 0);
            }
        }
        self.changing_rows.set(false);
        unsafe {
            let _ = InvalidateRect(Some(pane.list), None, false);
        }
        self.status_changed();
    }
    fn start_transfer(&self, action: Transfer) {
        let index = self.active.get();
        let pane = &self.panes[index];
        if self.operation_active.get() || !pane.listing.borrow().ready() {
            return;
        }
        let destination = {
            let target = self.panes[1 - index].listing.borrow();
            if !target.complete || target.error.is_some() {
                return;
            }
            target.path.clone()
        };
        let (folder, sources) = pane.selected_paths();
        if sources.is_empty() || folder == destination {
            return;
        }
        let text = format!(
            "{} {} to\n{}?",
            action.verb(),
            selection_label(sources.len()),
            destination.display()
        );
        if message(
            self.window,
            "Between panes",
            &text,
            MB_OKCANCEL | MB_ICONQUESTION | MB_DEFBUTTON2,
        ) == IDOK
        {
            let mut changes = Changes::folder(folder);
            changes.folders.push(destination.clone());
            self.submit(
                Operation::Transfer {
                    action,
                    sources,
                    destination,
                },
                changes,
            );
        }
        self.focus_list(index);
    }

    fn rename(&self) {
        let index = self.active.get();
        let pane = &self.panes[index];
        if self.operation_active.get() || !pane.listing.borrow().ready() || pane.count() != 1 {
            return;
        }
        let Some((source, directory)) = pane
            .selection()
            .next()
            .and_then(|row| pane.item(row as isize))
        else {
            return;
        };
        let Some(original) = source.file_name() else {
            return;
        };
        let stem = if directory {
            original
        } else {
            source.file_stem().unwrap_or(original)
        };
        let name = prompt::name(
            self.window,
            self.font.get(),
            w!("Rename"),
            original,
            stem.encode_wide().count(),
        );
        self.focus_list(index);
        if let Some(name) = name
            && name != original
        {
            let mut changes = Changes::folder(pane.listing.borrow().path.clone());
            changes.rename = Some((wide(original), wide(&name)));
            self.submit(Operation::Rename { source, name }, changes);
        }
    }

    fn create_folder(&self) {
        let index = self.active.get();
        let pane = &self.panes[index];
        let (folder, location, epoch) = {
            let listing = pane.listing.borrow();
            if self.operation_active.get() || !listing.ready() {
                return;
            }
            (
                listing.path.clone(),
                listing.location,
                listing.selection_epoch,
            )
        };
        let name = prompt::name(
            self.window,
            self.font.get(),
            w!("New folder"),
            "New folder".as_ref(),
            10,
        );
        self.focus_list(index);
        if let Some(name) = name {
            let mut changes = Changes::folder(folder.clone());
            changes.reveal = Some((index, location, epoch, wide(&name)));
            self.submit(Operation::CreateFolder(folder.join(name)), changes);
        }
    }

    fn recycle(&self) {
        let index = self.active.get();
        let pane = &self.panes[index];
        if self.operation_active.get() || !pane.listing.borrow().ready() {
            return;
        }
        let (folder, sources) = pane.selected_paths();
        if sources.is_empty() {
            return;
        }
        let text = format!(
            "Send {} to the Recycle Bin?\n\n{}",
            selection_label(sources.len()),
            folder.display(),
        );
        if message(
            self.window,
            "Recycle selection",
            &text,
            MB_OKCANCEL | MB_ICONQUESTION | MB_DEFBUTTON2,
        ) == IDOK
        {
            self.submit(Operation::Recycle(sources), Changes::folder(folder));
        }
        self.focus_list(index);
    }

    fn submit(&self, operation: Operation, changes: Changes) {
        self.operation_active.set(true);
        let verb = operation.verb();
        *self.operation_status.borrow_mut() = format!("{verb} in progress…");
        self.status_changed();
        let sender = self.sender.clone();
        let owner = self.window.0 as usize;
        if let Err(error) = thread::Builder::new()
            .name("file-operation".into())
            .spawn(move || {
                let result = operation.perform(owner);
                if sender.send(Event::Operation(verb, changes, result)).is_ok() {
                    wake(owner);
                }
            })
        {
            self.finish_operation(
                verb,
                Changes {
                    folders: Vec::new(),
                    rename: None,
                    reveal: None,
                },
                Err(windows::core::Error::new(E_FAIL, error.to_string())),
            );
        }
    }

    fn finish_operation(&self, verb: &str, changes: Changes, result: Result<bool>) {
        self.operation_active.set(false);
        let completed = matches!(&result, Ok(false));
        *self.operation_status.borrow_mut() = match &result {
            Ok(false) => format!("{verb} finished"),
            Ok(true) => format!("{verb} stopped"),
            Err(_) => format!("{verb} failed"),
        };
        for index in 0..2 {
            let mut listing = self.panes[index].listing.borrow_mut();
            if !changes.folders.contains(&listing.path) {
                continue;
            }
            if completed {
                if let Some(rename) = &changes.rename {
                    add_renames(&mut listing.pending_renames, vec![rename.clone()]);
                }
                if let Some((pane, location, epoch, name)) = &changes.reveal
                    && index == *pane
                    && listing.location == *location
                {
                    listing.pending_reveal = Some((*epoch, name.clone()));
                }
            }
            drop(listing);
            self.refresh(index);
        }
        self.status_changed();
        if let Err(error) = result {
            message(
                self.window,
                &format!("{verb} failed"),
                &error.message(),
                MB_OK | MB_ICONWARNING,
            );
        }
    }
    fn events(&self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Directory(Update::Directory {
                    pane,
                    id,
                    preview,
                    entries,
                }) => self.receive_directory(pane, id, preview, entries),
                Event::Directory(Update::View { pane, id, rows }) => {
                    if self.panes[pane].listing.borrow().view_request == id {
                        self.apply_rows(pane, rows);
                    }
                }
                Event::Watch(index, id, notice) => self.receive_watch(index, id, notice),
                Event::Operation(verb, changes, result) => {
                    self.finish_operation(verb, changes, result)
                }
            }
        }
    }

    fn command(&self, command: u16) {
        match command {
            COPY => self.start_transfer(Transfer::Copy),
            MOVE => self.start_transfer(Transfer::Move),
            RENAME => self.rename(),
            CREATE_FOLDER => self.create_folder(),
            RECYCLE => self.recycle(),
            REFRESH => self.refresh(self.active.get()),
            LEFT_CLEAR => self.clear_filter(0),
            RIGHT_CLEAR => self.clear_filter(1),
            SORT_NAME..=SORT_MODIFIED => {
                let column = Column::from_index((command - SORT_NAME) as usize).unwrap();
                self.sort_by(self.active.get(), column);
            }
            VIEW => self.view_menu(),
            LEFT_UP => self.parent(0),
            RIGHT_UP => self.parent(1),
            SYSTEM | LIGHT | DARK => {
                self.theme.set(match command {
                    LIGHT => Theme::Light,
                    DARK => Theme::Dark,
                    _ => Theme::System,
                });
                self.apply_theme();
            }
            _ => {}
        }
    }

    fn view_menu(&self) {
        let mut button = RECT::default();
        let sort = self.panes[self.active.get()].listing.borrow().sort;
        unsafe {
            let _ = CheckMenuRadioItem(
                self.menu,
                SORT_NAME as u32,
                SORT_MODIFIED as u32,
                SORT_NAME as u32 + sort.column.index() as u32,
                MF_BYCOMMAND.0,
            );
            let _ = GetWindowRect(self.view, &mut button);
            let _ = TrackPopupMenu(
                self.menu,
                TPM_RIGHTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON,
                button.right,
                button.top,
                Some(0),
                self.window,
                None,
            );
        }
        if unsafe { GetFocus() } == self.view {
            self.focus_list(self.active.get());
        }
    }

    fn key(&self, control: HWND, key: u16) -> bool {
        let active = self.active.get();
        let pane = &self.panes[active];
        let in_edit = control == pane.path;
        let filtering = control == pane.filter;
        let in_list = control == pane.list;
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        match VIRTUAL_KEY(key) {
            VK_TAB => self.focus_list(1 - active),
            VK_L if ctrl => unsafe {
                let _ = SetFocus(Some(pane.path));
                send(pane.path, EM_SETSEL, 0, -1);
            },
            VK_F if ctrl => unsafe {
                let _ = SetFocus(Some(pane.filter));
                send(pane.filter, EM_SETSEL, 0, -1);
            },
            VK_R if ctrl => self.refresh(active),
            VK_1 if ctrl => self.sort_by(active, Column::Name),
            VK_2 if ctrl => self.sort_by(active, Column::Size),
            VK_3 if ctrl => self.sort_by(active, Column::Modified),
            VK_A if ctrl && in_list => {
                if pane.listing.borrow().ready() {
                    item_state(pane.list, usize::MAX, LVIS_SELECTED, LVIS_SELECTED);
                }
            }
            VK_BACK if in_list => self.parent(active),
            VK_F5 => self.start_transfer(Transfer::Copy),
            VK_F6 => self.start_transfer(Transfer::Move),
            VK_F2 if in_list => self.rename(),
            VK_F7 => self.create_folder(),
            VK_DELETE if in_list => self.recycle(),
            VK_RETURN if in_edit => {
                let path = window_text(pane.path);
                if !path.is_empty() {
                    let base = pane.listing.borrow().path.clone();
                    self.navigate(active, base.join(Path::new(&path)));
                    self.focus_list(active);
                }
            }
            VK_ESCAPE if in_edit => {
                show_path(pane.path, &pane.listing.borrow().path);
                self.focus_list(active);
            }
            VK_ESCAPE if filtering => self.clear_filter(active),
            VK_RETURN if filtering => self.focus_list(active),
            VK_RETURN if in_list => {
                let selected = send(
                    pane.list,
                    LVM_GETNEXTITEM,
                    usize::MAX,
                    LVNI_FOCUSED as isize,
                );
                self.open_row(active, selected);
            }
            _ => return false,
        }
        true
    }

    fn notify(&self, data: LPARAM) -> LRESULT {
        let header = unsafe { &*(data.0 as *const NMHDR) };
        let Some(index) = self
            .panes
            .iter()
            .position(|pane| pane.list == header.hwndFrom)
        else {
            return LRESULT(0);
        };
        let pane = &self.panes[index];
        match header.code {
            LVN_GETDISPINFOW => {
                let request = unsafe { &mut *(data.0 as *mut NMLVDISPINFOW) };
                let item = &mut request.item;
                let listing = pane.listing.borrow();
                if let Some(entry) = listing.rows.get(item.iItem as usize) {
                    if item.mask.contains(LVIF_IMAGE) {
                        item.iImage = self.icons.as_ref().map_or(-1, |icons| {
                            if entry.directory {
                                icons.folder
                            } else {
                                icons.file
                            }
                        });
                    }
                    if item.mask.contains(LVIF_TEXT) {
                        if item.iSubItem == 0 {
                            copy_text(item, &entry.name);
                        } else {
                            copy_text(item, &wide(entry.detail(item.iSubItem)));
                        }
                    }
                } else if item.mask.contains(LVIF_TEXT) {
                    copy_text(item, &[0]);
                }
            }
            LVN_ODFINDITEMW => {
                let info = unsafe { &*(data.0 as *const NMLVFINDITEMW) };
                if info.lvfi.psz.is_null() || !info.lvfi.flags.contains(LVFI_STRING) {
                    return LRESULT(-1);
                }
                let query = unsafe { info.lvfi.psz.as_wide() };
                let listing = pane.listing.borrow();
                let start = (info.iStart.max(0) as usize).min(listing.rows.len());
                let result = (start..listing.rows.len()).chain(0..start).find(|&index| {
                    let name = &listing.rows.get(index).unwrap().name;
                    name.len() > query.len()
                        && unsafe { CompareStringOrdinal(&name[..query.len()], query, true) }
                            == CSTR_EQUAL
                });
                return LRESULT(result.map_or(-1, |index| index as isize));
            }
            NM_SETFOCUS => self.active(index),
            LVN_COLUMNCLICK => {
                let click = unsafe { &*(data.0 as *const NMLISTVIEW) };
                if let Some(column) = Column::from_index(click.iSubItem as usize) {
                    self.sort_by(index, column);
                }
            }
            NM_DBLCLK => {
                let info = unsafe { &*(data.0 as *const NMITEMACTIVATE) };
                self.open_row(index, info.iItem as isize);
            }
            LVN_ITEMCHANGED | LVN_ODSTATECHANGED if !self.changing_rows.get() => {
                let mut listing = pane.listing.borrow_mut();
                listing.selection_epoch = listing.selection_epoch.wrapping_add(1);
                drop(listing);
                self.status_changed()
            }
            _ => {}
        }
        LRESULT(0)
    }

    fn save_workspace(&self) {
        if self.persist {
            let left = self.panes[0].listing.borrow();
            let right = self.panes[1].listing.borrow();
            let settings = Workspace {
                paths: [left.path.clone(), right.path.clone()],
                sort: [left.sort, right.sort],
                theme: self.theme.get(),
                split: self.split.get(),
                persist: true,
            };
            let _ = settings.save();
        }
    }
}

fn add_renames(renames: &mut Vec<Rename>, incoming: Vec<Rename>) {
    if renames.len() + incoming.len() > 128 {
        renames.clear();
    } else {
        renames.extend(incoming);
    }
}

fn selection_label(count: usize) -> String {
    if count == 1 {
        "1 selected item".to_owned()
    } else {
        format!("{} selected items", grouped(count as u64))
    }
}

fn renamed<'a>(mut name: &'a [u16], pairs: &'a [Rename]) -> &'a [u16] {
    for (previous, next) in pairs {
        if name == previous {
            name = next;
        }
    }
    name
}

fn scroll_to(list: HWND, row: usize) {
    let current = send(list, LVM_GETTOPINDEX, 0, 0);
    let mut rect = RECT {
        left: LVIR_BOUNDS as i32,
        ..Default::default()
    };
    if send(
        list,
        LVM_GETITEMRECT,
        current.max(0) as usize,
        (&mut rect as *mut RECT) as isize,
    ) != 0
    {
        let distance = (row as isize - current) * (rect.bottom - rect.top) as isize;
        send(list, LVM_SCROLL, 0, distance);
    }
}

fn parent_of(path: &Path) -> Option<PathBuf> {
    path.parent()
        .filter(|parent| parent.has_root())
        .map(Path::to_path_buf)
}

fn send(window: HWND, message: u32, wparam: usize, lparam: isize) -> isize {
    unsafe { SendMessageW(window, message, Some(WPARAM(wparam)), Some(LPARAM(lparam))).0 }
}

fn item_state(
    window: HWND,
    index: usize,
    state: LIST_VIEW_ITEM_STATE_FLAGS,
    mask: LIST_VIEW_ITEM_STATE_FLAGS,
) {
    let item = LVITEMW {
        state,
        stateMask: mask,
        ..Default::default()
    };
    send(
        window,
        LVM_SETITEMSTATE,
        index,
        (&item as *const LVITEMW) as isize,
    );
}

fn copy_text(item: &LVITEMW, text: &[u16]) {
    if item.pszText.is_null() || item.cchTextMax <= 0 {
        return;
    }
    let length = (item.cchTextMax as usize - 1).min(text.len() - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), item.pszText.as_ptr(), length);
        *item.pszText.as_ptr().add(length) = 0;
    }
}

fn wake(owner: usize) {
    unsafe {
        let _ = PostMessageW(Some(HWND(owner as _)), WAKE, WPARAM(0), LPARAM(0));
    }
}

fn window_text(window: HWND) -> OsString {
    unsafe {
        let mut text = vec![0; GetWindowTextLengthW(window) as usize + 1];
        let length = GetWindowTextW(window, &mut text) as usize;
        OsString::from_wide(&text[..length])
    }
}

fn show_path(control: HWND, path: &Path) {
    let text = wide(path);
    let end = text.len() - 1;
    unsafe {
        let _ = SetWindowTextW(control, PCWSTR(text.as_ptr()));
    }
    send(control, EM_SETSEL, end, end as isize);
    send(control, EM_SCROLLCARET, 0, 0);
}

fn point(data: LPARAM) -> POINT {
    POINT {
        x: data.0 as i16 as i32,
        y: (data.0 >> 16) as i16 as i32,
    }
}

unsafe extern "system" fn control_proc(
    window: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    unsafe {
        let app = || (GetWindowLongPtrW(HWND(data as _), GWLP_USERDATA) as *const App).as_ref();
        match msg {
            WM_NOTIFY => {
                let header = &*(lparam.0 as *const NMHDR);
                if header.code == NM_CUSTOMDRAW
                    && header.hwndFrom.0 as isize == send(window, LVM_GETHEADER, 0, 0)
                    && let Some(app) = app()
                {
                    return app.draw_header(window, &*(lparam.0 as *const NMCUSTOMDRAW));
                }
            }
            WM_NCDESTROY => {
                let _ = RemoveWindowSubclass(window, Some(control_proc), id);
            }
            _ => {}
        }
        DefSubclassProc(window, msg, wparam, lparam)
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        let app = (GetWindowLongPtrW(window, GWLP_USERDATA) as *const App).as_ref();
        if let Some(app) = app {
            match msg {
                WAKE => {
                    app.events();
                    return LRESULT(0);
                }
                WM_NOTIFY => return app.notify(lparam),
                WM_COMMAND => {
                    let id = wparam.0 as u16;
                    let code = (wparam.0 >> 16) as u16;
                    if code == EN_SETFOCUS as u16 && (131..=132).contains(&id) {
                        app.active((id - 131) as usize);
                    } else if code == EN_SETFOCUS as u16
                        && (LEFT_FILTER..=RIGHT_FILTER).contains(&id)
                    {
                        app.active((id - LEFT_FILTER) as usize);
                    } else if code == EN_CHANGE as u16 && (LEFT_FILTER..=RIGHT_FILTER).contains(&id)
                    {
                        app.filter_changed((id - LEFT_FILTER) as usize);
                    } else if code == 0 {
                        app.command(id);
                    }
                    return LRESULT(0);
                }
                WM_PAINT => {
                    app.paint();
                    return LRESULT(0);
                }
                WM_ERASEBKGND => return LRESULT(1),
                WM_DRAWITEM => {
                    app.draw_button(&*(lparam.0 as *const DRAWITEMSTRUCT));
                    return LRESULT(1);
                }
                WM_CTLCOLOREDIT => {
                    let palette = app.palette.borrow();
                    let dc = HDC(wparam.0 as _);
                    SetTextColor(dc, palette.text);
                    SetBkColor(dc, palette.surface);
                    return LRESULT(palette.surface_brush.0 as isize);
                }
                WM_SIZE => {
                    app.layout();
                    return LRESULT(0);
                }
                WM_TIMER if (SCAN_TIMER..SCAN_TIMER + 2).contains(&wparam.0) => {
                    app.start_initial_scan(wparam.0 - SCAN_TIMER);
                    return LRESULT(0);
                }
                WM_TIMER if (REFRESH_TIMER..REFRESH_TIMER + 2).contains(&wparam.0) => {
                    app.scan_dirty_folder(wparam.0 - REFRESH_TIMER);
                    return LRESULT(0);
                }
                WM_THEMECHANGED | WM_SETTINGCHANGE | WM_SYSCOLORCHANGE => app.apply_theme(),
                WM_DPICHANGED => {
                    app.dpi.set((wparam.0 as u16) as u32);
                    app.update_font();
                    let size = &*(lparam.0 as *const RECT);
                    let _ = SetWindowPos(
                        window,
                        None,
                        size.left,
                        size.top,
                        size.right - size.left,
                        size.bottom - size.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                    app.layout();
                    return LRESULT(0);
                }
                WM_GETMINMAXINFO => {
                    let minmax = &mut *(lparam.0 as *mut MINMAXINFO);
                    minmax.ptMinTrackSize.x = app.scale(770);
                    minmax.ptMinTrackSize.y = app.scale(440);
                    return LRESULT(0);
                }
                WM_LBUTTONDOWN if PtInRect(&app.layout.get().gutter, point(lparam)).as_bool() => {
                    app.dragging.set(true);
                    SetCapture(window);
                    return LRESULT(0);
                }
                WM_MOUSEMOVE if app.dragging.get() => {
                    let mut rect = RECT::default();
                    let _ = GetClientRect(window, &mut rect);
                    let width = rect.right - 2 * app.scale(16) - app.scale(14);
                    let split = (point(lparam).x - app.scale(16) - app.scale(7)) as f32
                        / width.max(1) as f32;
                    app.split.set(split.clamp(0.30, 0.70));
                    app.layout();
                    return LRESULT(0);
                }
                WM_LBUTTONUP if app.dragging.get() => {
                    let _ = ReleaseCapture();
                    return LRESULT(0);
                }
                WM_CAPTURECHANGED => app.dragging.set(false),
                WM_SETCURSOR if lparam.0 as u16 == HTCLIENT as u16 => {
                    let mut cursor = POINT::default();
                    let _ = GetCursorPos(&mut cursor);
                    let _ = ScreenToClient(window, &mut cursor);
                    if (app.dragging.get() || PtInRect(&app.layout.get().gutter, cursor).as_bool())
                        && let Ok(cursor) = LoadCursorW(None, IDC_SIZEWE)
                    {
                        SetCursor(Some(cursor));
                        return LRESULT(1);
                    }
                }
                WM_CLOSE if app.operation_active.get() => {
                    message(
                        window,
                        "File operation in progress",
                        "Finish or cancel the Windows file operation before closing Dual Pane.",
                        MB_OK | MB_ICONINFORMATION,
                    );
                    return LRESULT(0);
                }
                WM_CLOSE => app.save_workspace(),
                WM_NCDESTROY => {
                    SetWindowLongPtrW(window, GWLP_USERDATA, 0);
                }
                WM_DESTROY => {
                    if let Some(service) = &app.accessibility {
                        for pane in &app.panes {
                            for control in [pane.path, pane.list] {
                                let _ = service.ClearHwndProps(
                                    control,
                                    OBJID_CLIENT.0 as u32,
                                    CHILDID_SELF,
                                    &[Name_Property_GUID, PROPID_ACC_NAME],
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if msg == WM_DESTROY {
            PostQuitMessage(0);
            return LRESULT(0);
        }
        DefWindowProcW(window, msg, wparam, lparam)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_notification_resizes_the_native_controls() -> Result<()> {
        let _apartment = shell::Apartment::enter()?;
        let window = create_window()?;
        let app = attach(window, &Workspace::default())?;
        let target_dpi = 144;
        let size = RECT {
            left: 40,
            top: 40,
            right: 1600,
            bottom: 980,
        };
        let mut paths = [RECT::default(); 2];
        let mut content = [RECT::default(); 2];
        unsafe {
            SendMessageW(
                window,
                WM_DPICHANGED,
                Some(WPARAM(target_dpi | (target_dpi << 16))),
                Some(LPARAM((&size as *const RECT) as isize)),
            );
            for index in 0..2 {
                GetWindowRect(app.panes[index].path, &mut paths[index])?;
                GetWindowRect(app.panes[index].list, &mut content[index])?;
            }
            DestroyWindow(window)?;
        }
        assert_eq!(app.dpi.get(), target_dpi as u32);
        for path in paths {
            assert_eq!(path.bottom - path.top, 30);
            assert!(path.right - path.left > 480);
        }
        assert!(content[0].right < content[1].left);
        assert_eq!(content[0].top, content[1].top);
        assert!(content[0].bottom - content[0].top > 500);
        Ok(())
    }
}
