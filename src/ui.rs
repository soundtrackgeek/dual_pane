use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    ffi::{OsString, c_void},
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
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
    directory::{Entry, Scanner, Update, grouped, wide},
    shell::{self, Icons, Transfer},
};

mod paint;

const WAKE: u32 = WM_APP + 1;
const COPY: u16 = 101;
const MOVE: u16 = 102;
const REFRESH: u16 = 103;
const VIEW: u16 = 104;
const SYSTEM: u16 = 110;
const LIGHT: u16 = 111;
const DARK: u16 = 112;
const LEFT_UP: u16 = 121;
const RIGHT_UP: u16 = 122;

enum Event {
    Directory(Update),
    Transfer(Transfer, [PathBuf; 2], Result<bool>),
}

struct Listing {
    path: PathBuf,
    request: usize,
    rows: Vec<Entry>,
    loading: bool,
    error: Option<String>,
}

struct Pane {
    path: HWND,
    up: HWND,
    list: HWND,
    scanner: Scanner,
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
    transfer_active: Cell<bool>,
    transfer_status: RefCell<String>,
    events: Receiver<Event>,
    sender: Sender<Event>,
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
    unsafe {
        let window = create_window()?;
        let app = attach(window)?;
        let (left, right) = initial_paths();
        app.navigate(0, left, false);
        app.navigate(1, right, false);
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

fn attach(window: HWND) -> Result<Box<App>> {
    let app = Box::new(App::new(window)?);
    unsafe {
        SetWindowLongPtrW(window, GWLP_USERDATA, (&*app as *const App) as isize);
    }
    app.update_font();
    app.apply_theme();
    app.layout();
    Ok(app)
}
fn initial_paths() -> (PathBuf, PathBuf) {
    let mut args = std::env::args_os().skip(1);
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from(r"C:\"));
    let left = args
        .next()
        .map(PathBuf::from)
        .or(home)
        .unwrap_or_else(|| current.clone());
    let right = args.next().map(PathBuf::from).unwrap_or(current);
    (left, right)
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
        (panes[0].list, w!("Left folder contents")),
        (panes[1].path, w!("Right folder path")),
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
    ) -> Result<Self> {
        let path = child(
            window,
            w!("Edit"),
            PCWSTR::null(),
            WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            131 + index as u16,
        )?;
        let up = button(window, w!("Up"), LEFT_UP + index as u16)?;
        let list = child(
            window,
            WC_LISTVIEWW,
            PCWSTR::null(),
            WS_TABSTOP
                | WINDOW_STYLE(
                    LVS_REPORT
                        | LVS_OWNERDATA
                        | LVS_SHOWSELALWAYS
                        | LVS_NOSORTHEADER
                        | LVS_SHAREIMAGELISTS,
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
        let scanner = Scanner::start(index, move |update| {
            if sender.send(Event::Directory(update)).is_ok() {
                wake(owner);
            }
        })
        .map_err(|error| windows::core::Error::new(E_FAIL, error.to_string()))?;
        Ok(Self {
            path,
            up,
            list,
            scanner,
            listing: RefCell::new(Listing {
                path: PathBuf::new(),
                request: 0,
                rows: Vec::new(),
                loading: true,
                error: None,
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
    fn new(window: HWND) -> Result<Self> {
        let (sender, events) = mpsc::channel();
        let icons = Icons::load();
        let panes = [
            Pane::new(window, 0, sender.clone(), icons.as_ref())?,
            Pane::new(window, 1, sender.clone(), icons.as_ref())?,
        ];
        let copy = button(window, w!("F5   Copy"), COPY)?;
        let move_ = button(window, w!("F6   Move"), MOVE)?;
        let view = button(window, w!("View   ▾"), VIEW)?;
        let menu = unsafe {
            let menu = CreatePopupMenu()?;
            menu_label(menu, REFRESH, w!("&Refresh active pane\tCtrl+R"))?;
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
            view,
            menu,
            accessibility,
            active: Cell::new(0),
            dpi: Cell::new(unsafe { GetDpiForWindow(window) }.max(96)),
            font: Cell::new(HFONT::default()),
            theme: Cell::new(Theme::System),
            palette: RefCell::new(paint::Palette::new(Theme::System)),
            layout: Cell::new(Layout::default()),
            split: Cell::new(0.5),
            dragging: Cell::new(false),
            changing_rows: Cell::new(false),
            icons,
            transfer_active: Cell::new(false),
            transfer_status: RefCell::new(String::new()),
            events,
            sender,
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
        let enabled = !self.transfer_active.get()
            && !destination.loading
            && destination.error.is_none()
            && !origin.loading
            && origin.path != destination.path
            && self.panes[source].count() > 0;
        drop(origin);
        drop(destination);
        unsafe {
            let _ = EnableWindow(self.copy, enabled);
            let _ = EnableWindow(self.move_, enabled);
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

    fn navigate(&self, index: usize, path: PathBuf, refresh: bool) {
        let pane = &self.panes[index];
        let absolute = std::path::absolute(path);
        let Ok(path) = absolute else {
            let error = absolute.unwrap_err();
            message(
                self.window,
                "Invalid path",
                &error.to_string(),
                MB_OK | MB_ICONWARNING,
            );
            return;
        };
        let id = {
            let mut listing = pane.listing.borrow_mut();
            listing.request = listing.request.wrapping_add(1);
            listing.loading = true;
            listing.error = None;
            listing.path.clone_from(&path);
            if !refresh {
                listing.rows = Vec::new();
            }
            listing.request
        };
        if !refresh {
            self.changing_rows.set(true);
            send(pane.list, LVM_SETITEMCOUNT, 0, 0);
            self.changing_rows.set(false);
            show_path(pane.path, &path);
        }
        let parent = parent_of(&path).is_some();
        unsafe {
            let _ = EnableWindow(pane.up, parent);
        }
        pane.scanner.request(id, path, !refresh);
        self.status_changed();
    }

    fn parent(&self, index: usize) {
        let parent = parent_of(&self.panes[index].listing.borrow().path);
        if let Some(parent) = parent {
            self.navigate(index, parent, false);
        }
        self.focus_list(index);
    }

    fn refresh(&self, index: usize) {
        let path = self.panes[index].listing.borrow().path.clone();
        self.navigate(index, path, true);
    }

    fn open_row(&self, index: usize, row: isize) {
        if let Some((path, directory)) = self.panes[index].item(row) {
            if directory {
                self.navigate(index, path, false);
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

    fn receive_directory(&self, update: Update) {
        let pane = &self.panes[update.pane];
        if pane.listing.borrow().request != update.id {
            return;
        }
        let focused = send(
            pane.list,
            LVM_GETNEXTITEM,
            usize::MAX,
            LVNI_FOCUSED as isize,
        );
        let top = send(pane.list, LVM_GETTOPINDEX, 0, 0);
        let (selected, focused, top) = {
            let listing = pane.listing.borrow();
            let name_at =
                |index: isize| listing.rows.get(index as usize).map(|row| row.name.clone());
            let selected = pane
                .selection()
                .filter_map(|index| name_at(index as isize))
                .collect::<HashSet<_>>();
            (selected, name_at(focused), name_at(top))
        };
        let mut listing = pane.listing.borrow_mut();
        listing.loading = update.preview;
        match update.entries {
            Ok(rows) => listing.rows = rows,
            Err(error) => {
                listing.rows = Vec::new();
                listing.error = Some(error.message().trim().to_owned());
            }
        }
        let total = listing.rows.len();
        let mut selected_rows = Vec::new();
        let mut focused_row = None;
        let mut top_row = None;
        if !selected.is_empty() || focused.is_some() || top.is_some() {
            for (index, row) in listing.rows.iter().enumerate() {
                if selected.contains(&row.name) {
                    selected_rows.push(index);
                }
                if focused.as_deref() == Some(&row.name) {
                    focused_row = Some(index);
                }
                if top.as_deref() == Some(&row.name) {
                    top_row = Some(index);
                }
            }
        }
        drop(listing);
        self.changing_rows.set(true);
        let selection_matches_all = total > 0 && selected_rows.len() == total;
        item_state(
            pane.list,
            usize::MAX,
            LIST_VIEW_ITEM_STATE_FLAGS(0),
            LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
        );
        send(pane.list, LVM_SETITEMCOUNT, total, LVSICF_NOSCROLL as isize);
        if selection_matches_all {
            item_state(pane.list, usize::MAX, LVIS_SELECTED, LVIS_SELECTED);
        } else {
            for row in selected_rows {
                item_state(pane.list, row, LVIS_SELECTED, LVIS_SELECTED);
            }
        }
        if total > 0 {
            let row = focused_row.unwrap_or(0);
            item_state(pane.list, row, LVIS_FOCUSED, LVIS_FOCUSED);
            if let Some(top) = top_row {
                send(pane.list, LVM_ENSUREVISIBLE, top, 0);
            }
            send(pane.list, LVM_ENSUREVISIBLE, row, 0);
        }
        self.changing_rows.set(false);
        unsafe {
            let _ = InvalidateRect(Some(pane.list), None, false);
        }
        self.status_changed();
    }

    fn start_transfer(&self, action: Transfer) {
        if self.transfer_active.get() {
            return;
        }
        let index = self.active.get();
        let pane = &self.panes[index];
        let destination = {
            let target = self.panes[1 - index].listing.borrow();
            if target.loading || target.error.is_some() {
                return;
            }
            target.path.clone()
        };
        let (source_path, sources) = {
            let source = pane.listing.borrow();
            if source.loading || source.path == destination {
                return;
            }
            let paths = pane
                .selection()
                .filter_map(|index| source.rows.get(index))
                .map(|row| source.path.join(row.filename()))
                .collect::<Vec<_>>();
            (source.path.clone(), paths)
        };
        if sources.is_empty() {
            return;
        }
        let count = grouped(sources.len() as u64);
        let text = format!(
            "{} {count} selected item(s) to\n{}?",
            action.verb(),
            destination.display()
        );
        if message(
            self.window,
            "Between panes",
            &text,
            MB_OKCANCEL | MB_ICONQUESTION | MB_DEFBUTTON2,
        ) != IDOK
        {
            self.focus_list(index);
            return;
        }
        self.transfer_active.set(true);
        *self.transfer_status.borrow_mut() = format!("{} in progress…", action.verb());
        self.focus_list(index);
        self.status_changed();
        let sender = self.sender.clone();
        let owner = self.window.0 as usize;
        let folders = [source_path, destination];
        let finished_folders = folders.clone();
        if let Err(error) = thread::Builder::new()
            .name("shell-transfer".into())
            .spawn(move || {
                let result = shell::transfer(owner as isize, action, &sources, &folders[1]);
                if sender
                    .send(Event::Transfer(action, folders, result))
                    .is_ok()
                {
                    wake(owner);
                }
            })
        {
            self.finish_transfer(
                action,
                &finished_folders,
                Err(windows::core::Error::new(E_FAIL, error.to_string())),
            );
        }
    }

    fn finish_transfer(&self, action: Transfer, folders: &[PathBuf; 2], result: Result<bool>) {
        self.transfer_active.set(false);
        *self.transfer_status.borrow_mut() = match result {
            Ok(false) => format!("{} finished", action.verb()),
            Ok(true) => format!(
                "{} stopped · some items may have transferred",
                action.verb()
            ),
            Err(error) => format!("{} failed · {}", action.verb(), error.message().trim()),
        };
        for index in 0..2 {
            // Supersede a scan that may have started before the transfer changed its folder.
            if folders.contains(&self.panes[index].listing.borrow().path) {
                self.refresh(index);
            }
        }
        self.status_changed();
    }

    fn events(&self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Directory(update) => self.receive_directory(update),
                Event::Transfer(action, folders, result) => {
                    self.finish_transfer(action, &folders, result)
                }
            }
        }
    }

    fn command(&self, command: u16) {
        match command {
            COPY => self.start_transfer(Transfer::Copy),
            MOVE => self.start_transfer(Transfer::Move),
            REFRESH => self.refresh(self.active.get()),
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
        unsafe {
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
        let in_list = control == pane.list;
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        match VIRTUAL_KEY(key) {
            VK_TAB => self.focus_list(1 - active),
            VK_L if ctrl => unsafe {
                let _ = SetFocus(Some(pane.path));
                send(pane.path, EM_SETSEL, 0, -1);
            },
            VK_R if ctrl => self.refresh(active),
            VK_A if ctrl && in_list => {
                if !pane.listing.borrow().loading {
                    item_state(pane.list, usize::MAX, LVIS_SELECTED, LVIS_SELECTED);
                }
            }
            VK_BACK if in_list => self.parent(active),
            VK_F5 => self.start_transfer(Transfer::Copy),
            VK_F6 => self.start_transfer(Transfer::Move),
            VK_RETURN if in_edit => {
                let path = window_text(pane.path);
                if !path.is_empty() {
                    let base = pane.listing.borrow().path.clone();
                    self.navigate(active, base.join(Path::new(&path)), false);
                    self.focus_list(active);
                }
            }
            VK_ESCAPE if in_edit => {
                show_path(pane.path, &pane.listing.borrow().path);
                self.focus_list(active);
            }
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
                    let name = &listing.rows[index].name;
                    name.len() > query.len()
                        && unsafe { CompareStringOrdinal(&name[..query.len()], query, true) }
                            == CSTR_EQUAL
                });
                return LRESULT(result.map_or(-1, |index| index as isize));
            }
            NM_SETFOCUS => self.active(index),
            NM_DBLCLK => {
                let info = unsafe { &*(data.0 as *const NMITEMACTIVATE) };
                self.open_row(index, info.iItem as isize);
            }
            LVN_ITEMCHANGED | LVN_ODSTATECHANGED if !self.changing_rows.get() => {
                self.status_changed()
            }
            _ => {}
        }
        LRESULT(0)
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
                    return app.draw_header(&*(lparam.0 as *const NMCUSTOMDRAW));
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
                WM_CLOSE if app.transfer_active.get() => {
                    message(
                        window,
                        "File operation in progress",
                        "Finish or cancel the Windows file operation before closing Dual Pane.",
                        MB_OK | MB_ICONINFORMATION,
                    );
                    return LRESULT(0);
                }
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
        let app = attach(window)?;
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
