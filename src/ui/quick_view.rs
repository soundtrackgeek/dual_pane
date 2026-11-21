use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::System::SystemServices::{SS_ENDELLIPSIS, SS_NOPREFIX, SS_OWNERDRAW};

use super::*;
use crate::preview::{self, Preview};

const NAME: u16 = 641;
const DETAILS: u16 = 642;
const TEXT: u16 = 643;
const CANVAS: u16 = 644;
const STATUS: u16 = 645;
const PREVIOUS: u16 = 646;
const NEXT: u16 = 647;
const OPEN: u16 = 648;
const CLOSE: u16 = IDCANCEL.0 as u16;
const MAX_WORKERS: usize = 2;

struct Item {
    pane: usize,
    location: usize,
    row: usize,
    path: PathBuf,
    archive_member: Option<String>,
    entry: Entry,
}

struct Request {
    id: usize,
    path: PathBuf,
    archive_member: Option<String>,
    current: Arc<AtomicBool>,
}

#[derive(Default)]
pub(super) struct QuickView {
    pub window: Cell<HWND>,
    item: RefCell<Option<Item>>,
    display: RefCell<Option<Preview>>,
    pending: RefCell<Option<Request>>,
    current: RefCell<Option<Arc<AtomicBool>>>,
    generation: Cell<usize>,
    workers: Cell<usize>,
    fonts: Cell<[HFONT; 3]>,
}

impl QuickView {
    fn clear(&self) {
        if let Some(current) = self.current.take() {
            current.store(false, Ordering::Relaxed);
        }
        self.pending.take();
        self.display.take();
    }
}

impl App {
    fn preview_item(&self, pane: usize, row: usize) -> Option<Item> {
        let listing = self.panes[pane].listing.borrow();
        if listing.projecting {
            return None;
        }
        let entry = listing.rows.get(row)?;
        if entry.directory {
            return None;
        }
        let filename = entry.filename();
        let (path, archive_member) = if let Some(folder) = &listing.archive {
            let name = filename.to_str()?;
            let member = if folder.is_empty() {
                name.to_owned()
            } else {
                format!("{folder}/{name}")
            };
            (listing.path.clone(), Some(member))
        } else {
            (listing.path.join(filename), None)
        };
        Some(Item {
            pane,
            location: listing.location,
            row,
            path,
            archive_member,
            entry: entry.clone(),
        })
    }

    pub fn show_quick_view(&self) {
        let pane = self.active.get();
        let row = send(
            self.panes[pane].list,
            LVM_GETNEXTITEM,
            usize::MAX,
            LVNI_FOCUSED as isize,
        );
        let Some(item) = usize::try_from(row)
            .ok()
            .and_then(|row| self.preview_item(pane, row))
        else {
            return;
        };
        let viewer = &self.quick_view;
        let mut panel = viewer.window.get();
        let first = panel.0.is_null();
        if first {
            match prompt::create_panel(
                self.window,
                [440, 286],
                WS_THICKFRAME | WS_MAXIMIZEBOX,
                Some(panel_proc),
                LPARAM((self as *const App) as isize),
            ) {
                Ok(window) if unsafe { IsWindow(Some(window)).as_bool() } => {
                    panel = window;
                    viewer.window.set(window);
                    self.theme_quick_view();
                }
                Ok(_) => return,
                Err(error) => {
                    message(
                        self.window,
                        "Could not open Quick View",
                        &error.message(),
                        MB_OK | MB_ICONWARNING,
                    );
                    return;
                }
            }
        }
        self.request_preview(panel, item);
        unsafe {
            if first {
                let _ = ShowWindow(panel, SW_SHOW);
            }
            let _ = SetForegroundWindow(panel);
            let _ = SetFocus(Some(control(panel, OPEN)));
        }
    }

    fn request_preview(&self, panel: HWND, item: Item) {
        let viewer = &self.quick_view;
        viewer.clear();
        let id = viewer.generation.get().wrapping_add(1);
        viewer.generation.set(id);
        let current = Arc::new(AtomicBool::new(true));
        let request = Request {
            id,
            path: item.path.clone(),
            archive_member: item.archive_member.clone(),
            current: current.clone(),
        };
        viewer.current.replace(Some(current));
        viewer.pending.replace(Some(request));
        let in_archive = item.archive_member.is_some();
        unsafe {
            let open = control(panel, OPEN);
            if in_archive && GetFocus() == open {
                let _ = SetFocus(Some(control(panel, CLOSE)));
            }
            let _ = EnableWindow(open, !in_archive);
        }
        set_text(control(panel, NAME), item.entry.filename());
        let modified = item.entry.detail(2);
        let size = format!("{} bytes", grouped(item.entry.size));
        set_text(
            control(panel, DETAILS),
            if modified.is_empty() {
                size
            } else {
                format!("{size}   ·   Modified {modified}")
            },
        );
        viewer.item.replace(Some(item));
        self.start_preview();
        self.render_preview(panel);
        self.update_quick_view_navigation();
    }

    fn start_preview(&self) {
        let viewer = &self.quick_view;
        let workers = viewer.workers.get();
        if workers >= MAX_WORKERS {
            return;
        }
        let Some(request) = viewer.pending.take() else {
            return;
        };
        let sender = self.sender.clone();
        let owner = self.window.0 as usize;
        match thread::Builder::new()
            .name("quick-view".into())
            .spawn(move || {
                let check = || request.current.load(Ordering::Relaxed);
                let preview = match &request.archive_member {
                    Some(member) => preview::load_archive(&request.path, member, check),
                    None => preview::load(&request.path, check),
                };
                if sender
                    .send(Event::PreviewFinished(request.id, preview))
                    .is_ok()
                {
                    wake(owner);
                }
            }) {
            Ok(_) => viewer.workers.set(workers + 1),
            Err(error) => {
                viewer.display.replace(Some(Preview::Details(format!(
                    "Could not start preview · {error}"
                ))));
            }
        }
    }

    pub fn receive_preview(&self, id: usize, preview: Option<Preview>) {
        let viewer = &self.quick_view;
        viewer.workers.set(viewer.workers.get() - 1);
        let panel = viewer.window.get();
        if panel.0.is_null() {
            return;
        }
        if id == viewer.generation.get() {
            viewer.display.replace(preview);
            self.render_preview(panel);
        } else if viewer.pending.borrow().is_some() {
            self.start_preview();
            self.render_preview(panel);
        }
    }

    fn render_preview(&self, panel: HWND) {
        let display = self.quick_view.display.borrow();
        let (text, status) = match display.as_ref() {
            Some(Preview::Text { content, excerpt }) => {
                let status = if *excerpt {
                    "Text excerpt · first 128 KiB"
                } else if content.is_empty() {
                    "Empty file"
                } else {
                    "Text preview"
                };
                (wide(content), status.into())
            }
            Some(Preview::Image(image)) => (
                Vec::new(),
                format!(
                    "Image preview · {} × {} px",
                    image.original.0, image.original.1
                ),
            ),
            Some(Preview::Details(reason)) => (Vec::new(), reason.clone()),
            None if self.quick_view.pending.borrow().is_some() => {
                (Vec::new(), "Waiting for an earlier read to finish…".into())
            }
            None => (Vec::new(), "Loading preview…".into()),
        };
        drop(display);
        let edit = control(panel, TEXT);
        let canvas = control(panel, CANVAS);
        let showing_text = !text.is_empty();
        unsafe {
            if !showing_text && GetFocus() == edit {
                let _ = SetFocus(Some(control(panel, OPEN)));
            }
            let _ = ShowWindow(edit, if showing_text { SW_SHOWNA } else { SW_HIDE });
            let _ = ShowWindow(canvas, if showing_text { SW_HIDE } else { SW_SHOWNA });
            let _ = SetWindowTextW(
                edit,
                if showing_text {
                    PCWSTR(text.as_ptr())
                } else {
                    w!("")
                },
            );
            let _ = InvalidateRect(Some(canvas), None, false);
        }
        set_text(control(panel, STATUS), status);
    }

    fn adjacent(&self, offset: isize) -> Option<(usize, usize)> {
        let viewer = self.quick_view.item.borrow();
        let item = viewer.as_ref()?;
        let listing = self.panes[item.pane].listing.borrow();
        if listing.location != item.location || listing.projecting {
            return None;
        }
        let matches = |entry: &Entry| entry.name == item.entry.name;
        let row = if listing.rows.get(item.row).is_some_and(matches) {
            item.row
        } else {
            listing.rows.iter().position(matches)?
        };
        // Folders are grouped before files in every supported sort.
        let row = row.checked_add_signed(offset)?;
        listing.rows.get(row).filter(|entry| !entry.directory)?;
        Some((item.pane, row))
    }

    pub fn update_quick_view_navigation(&self) {
        let panel = self.quick_view.window.get();
        if !panel.0.is_null() {
            unsafe {
                let _ = EnableWindow(control(panel, PREVIOUS), self.adjacent(-1).is_some());
                let _ = EnableWindow(control(panel, NEXT), self.adjacent(1).is_some());
            }
        }
    }

    fn step_preview(&self, panel: HWND, offset: isize) {
        if let Some((pane, row)) = self.adjacent(offset)
            && let Some(item) = self.preview_item(pane, row)
        {
            self.request_preview(panel, item);
        }
    }

    fn open_preview(&self, panel: HWND) {
        let path = self
            .quick_view
            .item
            .borrow()
            .as_ref()
            .filter(|item| item.archive_member.is_none())
            .map(|item| item.path.clone());
        if let Some(path) = path
            && let Err(error) = shell::open(panel, &path)
        {
            message(
                panel,
                "Could not open file",
                &error.message(),
                MB_OK | MB_ICONWARNING,
            );
        }
    }

    fn close_preview(&self, panel: HWND) {
        unsafe {
            let was_active = GetActiveWindow() == panel;
            let _ = DestroyWindow(panel);
            if was_active {
                self.focus_list(self.active.get());
            }
        }
    }

    pub fn quick_view_key(&self, panel: HWND, msg: &MSG) -> bool {
        if panel != self.quick_view.window.get()
            || !matches!(msg.message, WM_KEYDOWN | WM_SYSKEYDOWN)
        {
            return false;
        }
        let key = VIRTUAL_KEY(msg.wParam.0 as u16);
        if key == VK_ESCAPE {
            self.close_preview(panel);
            return true;
        }
        let alt = msg.message == WM_SYSKEYDOWN && msg.lParam.0 & (1 << 29) != 0;
        if !alt && (msg.message != WM_KEYDOWN || msg.hwnd == control(panel, TEXT)) {
            return false;
        }
        match key {
            VK_LEFT => self.step_preview(panel, -1),
            VK_RIGHT => self.step_preview(panel, 1),
            _ => return false,
        }
        true
    }

    pub fn theme_quick_view(&self) {
        let panel = self.quick_view.window.get();
        if panel.0.is_null() {
            return;
        }
        let dark = i32::from(self.palette.borrow().dark);
        unsafe {
            let _ = DwmSetWindowAttribute(
                panel,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                (&dark as *const i32).cast(),
                size_of::<i32>() as u32,
            );
            let _ = RedrawWindow(
                Some(panel),
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
            );
        }
    }

    fn draw_preview(&self, draw: &DRAWITEMSTRUCT) {
        let palette = self.palette.borrow();
        unsafe {
            FillRect(draw.hDC, &draw.rcItem, palette.surface_brush);
        }
        if let Some(Preview::Image(image)) = self.quick_view.display.borrow().as_ref() {
            let rect = draw.rcItem;
            let width = (rect.right - rect.left).max(1);
            let height = (rect.bottom - rect.top).max(1);
            let (w, h) = preview::fit(image.width, image.height, (width as u32, height as u32));
            let (w, h) = (w as i32, h as i32);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: image.width as i32,
                    biHeight: -(image.height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            unsafe {
                SetStretchBltMode(draw.hDC, HALFTONE);
                StretchDIBits(
                    draw.hDC,
                    rect.left + (width - w) / 2,
                    rect.top + (height - h) / 2,
                    w,
                    h,
                    0,
                    0,
                    image.width as i32,
                    image.height as i32,
                    Some(image.pixels.as_ptr().cast()),
                    &info,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
            }
        }
    }
}

fn control(panel: HWND, id: u16) -> HWND {
    unsafe { GetDlgItem(Some(panel), id as i32) }.unwrap_or_default()
}

fn set_text(window: HWND, text: impl AsRef<std::ffi::OsStr>) {
    let text = wide(text);
    unsafe {
        let _ = SetWindowTextW(window, PCWSTR(text.as_ptr()));
    }
}

fn scale(panel: HWND, value: i32) -> i32 {
    let dpi = unsafe { GetDpiForWindow(panel) }.max(96);
    (value * dpi as i32 + 48) / 96
}

fn layout(panel: HWND) {
    let mut client = RECT::default();
    if unsafe { GetClientRect(panel, &mut client) }.is_err() {
        return;
    }
    let s = |value| scale(panel, value);
    let margin = s(14);
    let width = (client.right - 2 * margin).max(1);
    let button = s(87);
    let gap = s(7);
    let footer = (client.bottom - s(42)).max(s(126));
    let body = (footer - s(30) - s(65)).max(1);
    let right = (client.right - margin - button).max(margin);
    for (id, x, y, w, h) in [
        (NAME, margin, s(10), width, s(25)),
        (DETAILS, margin, s(39), width, s(20)),
        (TEXT, margin, s(65), width, body),
        (CANVAS, margin, s(65), width, body),
        (STATUS, margin, footer - s(24), width, s(19)),
        (PREVIOUS, margin, footer, button, s(29)),
        (NEXT, margin + button + gap, footer, button, s(29)),
        (OPEN, right - button - gap, footer, button, s(29)),
        (CLOSE, right, footer, button, s(29)),
    ] {
        unsafe {
            let _ = MoveWindow(control(panel, id), x, y, w, h, true);
        }
    }
}

fn fonts(viewer: &QuickView, panel: HWND) {
    let font = |height, weight, name| unsafe {
        CreateFontW(
            -scale(panel, height),
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0.into(),
            name,
        )
    };
    let fonts = [
        font(15, FW_NORMAL.0 as i32, w!("Segoe UI")),
        font(20, FW_SEMIBOLD.0 as i32, w!("Segoe UI")),
        font(15, FW_NORMAL.0 as i32, w!("Consolas")),
    ];
    for id in [NAME, DETAILS, TEXT, STATUS, PREVIOUS, NEXT, OPEN, CLOSE] {
        let index = match id {
            NAME => 1,
            TEXT => 2,
            _ => 0,
        };
        send(control(panel, id), WM_SETFONT, fonts[index].0 as usize, 1);
    }
    delete_fonts(viewer.fonts.replace(fonts));
}

fn delete_fonts(fonts: [HFONT; 3]) {
    for font in fonts {
        if !font.0.is_null() {
            unsafe {
                let _ = DeleteObject(font.into());
            }
        }
    }
}

fn initialize(app: &App, panel: HWND) -> Result<()> {
    unsafe {
        SetWindowTextW(panel, w!("Quick View"))?;
    }
    let label = WINDOW_STYLE(SS_ENDELLIPSIS.0 | SS_NOPREFIX.0);
    let text = WS_TABSTOP
        | WS_HSCROLL
        | WS_VSCROLL
        | WINDOW_STYLE(
            (ES_MULTILINE | ES_READONLY | ES_AUTOHSCROLL | ES_AUTOVSCROLL | ES_NOHIDESEL) as u32,
        );
    let normal = WINDOW_EX_STYLE(0);
    let button = WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32);
    for (id, class, label_text, edge, style) in [
        (NAME, w!("Static"), w!(""), normal, label),
        (DETAILS, w!("Static"), w!(""), normal, label),
        (TEXT, w!("Edit"), w!(""), WS_EX_CLIENTEDGE, text),
        (
            CANVAS,
            w!("Static"),
            w!(""),
            WS_EX_CLIENTEDGE,
            WINDOW_STYLE(SS_OWNERDRAW.0),
        ),
        (STATUS, w!("Static"), w!(""), normal, label),
        (PREVIOUS, w!("Button"), w!("&Previous"), normal, button),
        (NEXT, w!("Button"), w!("&Next"), normal, button),
        (OPEN, w!("Button"), w!("&Open"), normal, button),
        (CLOSE, w!("Button"), w!("Close"), normal, button),
    ] {
        prompt::add_control(
            panel,
            HFONT::default(),
            (class, label_text, edge, style, id, [0, 0, 1, 1]),
        )?;
    }
    fonts(&app.quick_view, panel);
    layout(panel);
    send(
        control(panel, TEXT),
        EM_SETMARGINS,
        (EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize,
        (scale(panel, 5) << 16 | scale(panel, 5)) as isize,
    );
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
        match msg {
            WM_SIZE => layout(panel),
            WM_GETMINMAXINFO => {
                let info = &mut *(lparam.0 as *mut MINMAXINFO);
                info.ptMinTrackSize.x = scale(panel, 435);
                info.ptMinTrackSize.y = scale(panel, 245);
            }
            WM_DPICHANGED => {
                let rect = &*(lparam.0 as *const RECT);
                let _ = SetWindowPos(
                    panel,
                    None,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                fonts(&app.quick_view, panel);
                layout(panel);
            }
            WM_CTLCOLORDLG | WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT => {
                let palette = app.palette.borrow();
                let dc = HDC(wparam.0 as _);
                SetTextColor(dc, palette.text);
                SetBkColor(dc, palette.surface);
                return palette.surface_brush.0 as isize;
            }
            WM_DRAWITEM if wparam.0 == CANVAS as usize => {
                app.draw_preview(&*(lparam.0 as *const DRAWITEMSTRUCT))
            }
            WM_DRAWITEM => app.draw_button(
                &*(lparam.0 as *const DRAWITEMSTRUCT),
                app.quick_view.fonts.get()[0],
                true,
            ),
            WM_CLOSE => app.close_preview(panel),
            WM_COMMAND => match wparam.0 as u16 {
                PREVIOUS => app.step_preview(panel, -1),
                NEXT => app.step_preview(panel, 1),
                OPEN => app.open_preview(panel),
                CLOSE => app.close_preview(panel),
                _ => return 0,
            },
            WM_NCDESTROY => {
                let viewer = &app.quick_view;
                viewer.window.set(HWND::default());
                viewer.clear();
                viewer.item.take();
                delete_fonts(viewer.fonts.take());
                SetWindowLongPtrW(panel, GWLP_USERDATA, 0);
                return 0;
            }
            _ => return 0,
        }
    }
    1
}
