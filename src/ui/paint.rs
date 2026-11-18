use super::*;
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};

const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF(red as u32 | ((green as u32) << 8) | ((blue as u32) << 16))
}

pub struct Palette {
    pub dark: bool,
    contrast: bool,
    canvas: COLORREF,
    pub surface: COLORREF,
    strip: COLORREF,
    edge: COLORREF,
    pub text: COLORREF,
    muted: COLORREF,
    accent: COLORREF,
    active_strip: COLORREF,
    pub surface_brush: HBRUSH,
}

impl Palette {
    pub fn new(theme: Theme) -> Self {
        let mut light = 1_u32;
        let mut bytes = size_of::<u32>() as u32;
        let high_contrast = high_contrast();
        let dark = !high_contrast
            && match theme {
                Theme::Light => false,
                Theme::Dark => true,
                Theme::System => {
                    unsafe {
                        let _ = RegGetValueW(
                            HKEY_CURRENT_USER,
                            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
                            w!("AppsUseLightTheme"),
                            RRF_RT_REG_DWORD,
                            None,
                            Some((&mut light as *mut u32).cast()),
                            Some(&mut bytes),
                        );
                    }
                    light == 0
                }
            };
        let (canvas, surface, strip, edge, text, muted, accent, active_strip) = if high_contrast {
            unsafe {
                (
                    COLORREF(GetSysColor(COLOR_WINDOW)),
                    COLORREF(GetSysColor(COLOR_WINDOW)),
                    COLORREF(GetSysColor(COLOR_WINDOW)),
                    COLORREF(GetSysColor(COLOR_WINDOWTEXT)),
                    COLORREF(GetSysColor(COLOR_WINDOWTEXT)),
                    COLORREF(GetSysColor(COLOR_WINDOWTEXT)),
                    COLORREF(GetSysColor(COLOR_WINDOWTEXT)),
                    COLORREF(GetSysColor(COLOR_WINDOW)),
                )
            }
        } else if dark {
            (
                rgb(20, 24, 27),
                rgb(27, 32, 36),
                rgb(36, 43, 48),
                rgb(71, 85, 92),
                rgb(239, 244, 245),
                rgb(170, 185, 191),
                rgb(113, 218, 199),
                rgb(27, 58, 55),
            )
        } else {
            (
                rgb(237, 243, 243),
                rgb(255, 255, 255),
                rgb(245, 248, 248),
                rgb(154, 174, 177),
                rgb(28, 42, 47),
                rgb(75, 94, 99),
                rgb(0, 106, 93),
                rgb(218, 239, 234),
            )
        };
        Self {
            dark,
            contrast: high_contrast,
            canvas,
            surface,
            strip,
            edge,
            text,
            muted,
            accent,
            active_strip,
            surface_brush: unsafe { CreateSolidBrush(surface) },
        }
    }
}

impl Drop for Palette {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.surface_brush.into());
        }
    }
}

fn high_contrast() -> bool {
    let mut settings = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    let available = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            settings.cbSize,
            Some((&mut settings as *mut HIGHCONTRASTW).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS::default(),
        )
        .is_ok()
    };
    available && settings.dwFlags.0 & HCF_HIGHCONTRASTON.0 != 0
}
impl App {
    pub fn scale(&self, pixels: i32) -> i32 {
        (pixels * self.dpi.get() as i32 + 48) / 96
    }

    pub fn update_font(&self) {
        let font = unsafe {
            CreateFontW(
                -self.scale(15),
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                DEFAULT_PITCH.0 as u32,
                w!("Segoe UI"),
            )
        };
        if font.0.is_null() {
            return;
        }
        let previous = self.font.replace(font);
        for pane in &self.panes {
            for control in [
                pane.tabs,
                pane.new_tab,
                pane.close_tab,
                pane.back,
                pane.forward,
                pane.path,
                pane.up,
                pane.filter,
                pane.clear,
                pane.list,
            ] {
                send(control, WM_SETFONT, font.0 as usize, 0);
            }
            let header = send(pane.list, LVM_GETHEADER, 0, 0);
            if header != 0 {
                send(HWND(header as _), WM_SETFONT, font.0 as usize, 0);
            }
        }
        for control in [
            self.rename,
            self.copy,
            self.move_,
            self.create,
            self.recycle,
            self.pairs,
            self.transfer_button,
            self.view,
        ] {
            send(control, WM_SETFONT, font.0 as usize, 0);
        }
        self.update_find_font(font);
        self.update_batch_font(font);
        unsafe {
            let _ = DeleteObject(previous.into());
        }
    }

    pub fn apply_theme(&self) {
        let palette = Palette::new(self.theme.get());
        let dark = i32::from(palette.dark);
        let background = palette.surface.0 as isize;
        let foreground = palette.text.0 as isize;
        self.palette.replace(palette);
        self.theme_quick_view();
        unsafe {
            let _ = DwmSetWindowAttribute(
                self.window,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                (&dark as *const i32).cast(),
                size_of::<i32>() as u32,
            );
            let selection = match self.theme.get() {
                Theme::System => SYSTEM,
                Theme::Light => LIGHT,
                Theme::Dark => DARK,
            };
            let _ = CheckMenuRadioItem(
                self.menu,
                SYSTEM as u32,
                DARK as u32,
                selection as u32,
                MF_BYCOMMAND.0,
            );
            for pane in &self.panes {
                send(pane.list, LVM_SETBKCOLOR, 0, background);
                send(pane.list, LVM_SETTEXTBKCOLOR, 0, background);
                send(pane.list, LVM_SETTEXTCOLOR, 0, foreground);
            }
            let _ = RedrawWindow(
                Some(self.window),
                None,
                None,
                RDW_INVALIDATE | RDW_ALLCHILDREN,
            );
        }
    }

    pub fn layout(&self) {
        let s = |value| self.scale(value);
        let mut client = RECT::default();
        unsafe {
            if IsIconic(self.window).as_bool() || GetClientRect(self.window, &mut client).is_err() {
                return;
            }
        }
        let margin = s(16);
        let gap = s(14);
        let panel_width = (client.right - 2 * margin - gap).max(0);
        let left_width = (panel_width as f32 * self.split.get()) as i32;
        let middle = margin + left_width;
        let footer_top = (client.bottom - s(59)).max(0);
        let panes = [
            RECT {
                left: margin,
                right: middle,
                top: s(12),
                bottom: footer_top,
            },
            RECT {
                left: middle + gap,
                right: client.right - margin,
                top: s(12),
                bottom: footer_top,
            },
        ];
        let gutter = RECT {
            left: middle,
            right: middle + gap,
            top: s(12),
            bottom: footer_top,
        };
        let footer = RECT {
            left: margin,
            right: client.right - margin,
            top: footer_top,
            bottom: client.bottom,
        };
        self.layout.set(Layout {
            panes,
            gutter,
            footer,
        });

        for (pane, bounds) in self.panes.iter().zip(panes) {
            let left = bounds.left;
            let right = bounds.right;
            let strip = right - left - s(68);
            place(pane.tabs, left, bounds.top + s(27), strip, s(29));
            let tab_width = (strip - s(38)).clamp(s(75), s(126)) as isize;
            send(
                pane.tabs,
                TCM_SETITEMSIZE,
                0,
                tab_width | ((s(23) as isize) << 16),
            );
            place(
                pane.new_tab,
                right - s(62),
                bounds.top + s(28),
                s(29),
                s(26),
            );
            place(
                pane.close_tab,
                right - s(29),
                bounds.top + s(28),
                s(29),
                s(26),
            );
            place(pane.back, left, bounds.top + s(60), s(30), s(33));
            place(pane.forward, left + s(35), bounds.top + s(60), s(30), s(33));
            place(
                pane.path,
                left + s(81),
                bounds.top + s(68),
                right - left - s(147),
                s(20),
            );
            place(pane.up, right - s(53), bounds.top + s(60), s(53), s(33));
            place(
                pane.filter,
                left + s(59),
                bounds.top + s(102),
                right - left - s(136),
                s(20),
            );
            place(pane.clear, right - s(53), bounds.top + s(97), s(53), s(28));
            place(
                pane.list,
                left + 1,
                bounds.top + s(136),
                right - left - 2,
                bounds.bottom - bounds.top - s(166),
            );
            let slim = right - left < s(305);
            let size = if slim { s(61) } else { s(82) };
            let modified = if slim { s(122) } else { s(148) };
            // Reserve the scrollbar width so a newly populated list keeps its columns stable.
            let scroll = unsafe { GetSystemMetricsForDpi(SM_CXVSCROLL, self.dpi.get()) };
            let name = (right - left - size - modified - scroll - 3).max(s(55));
            for (column, width) in [name, size, modified].into_iter().enumerate() {
                send(pane.list, LVM_SETCOLUMNWIDTH, column, width as isize);
            }
        }
        let mut x = footer.left;
        let compact = footer.right - footer.left < s(880);
        for (button, wide, narrow) in [
            (self.rename, 104, 89),
            (self.copy, 85, 69),
            (self.move_, 85, 70),
            (self.create, 100, 85),
            (self.recycle, 103, 86),
        ] {
            let width = if compact { narrow } else { wide };
            place(button, x, footer.top + s(13), s(width), s(32));
            x += s(width + 7);
        }
        place(
            self.transfer_button,
            footer.right - s(275),
            footer.top + s(13),
            s(93),
            s(32),
        );
        place(
            self.pairs,
            footer.right - s(174),
            footer.top + s(13),
            s(78),
            s(32),
        );
        place(
            self.view,
            footer.right - s(88),
            footer.top + s(13),
            s(88),
            s(32),
        );
        unsafe {
            let _ = InvalidateRect(Some(self.window), None, false);
        }
    }

    pub fn paint(&self) {
        let mut paint = PAINTSTRUCT::default();
        unsafe {
            let dc = BeginPaint(self.window, &mut paint);
            self.paint_content(dc, paint.rcPaint);
            let _ = EndPaint(self.window, &paint);
        }
    }

    fn paint_content(&self, dc: HDC, invalid: RECT) {
        let p = self.palette.borrow();
        let layout = self.layout.get();
        let s = |value| self.scale(value);
        background(dc, invalid, p.canvas);
        unsafe {
            SelectObject(dc, self.font.get().into());
        }
        for (index, bounds) in layout.panes.into_iter().enumerate() {
            let active = index == self.active.get();
            let mut intersection = RECT::default();
            if !unsafe { IntersectRect(&mut intersection, &bounds, &invalid) }.as_bool() {
                continue;
            }
            let label_area = RECT {
                bottom: bounds.top + s(25),
                ..bounds
            };
            if active {
                background(dc, label_area, p.active_strip);
            }
            let accent = if active { p.accent } else { p.edge };
            background(
                dc,
                RECT {
                    bottom: bounds.top + s(2),
                    ..bounds
                },
                accent,
            );
            let heading = match (index, active) {
                (0, true) => "Left pane  ·  Active",
                (0, false) => "Left pane",
                (_, true) => "Right pane  ·  Active",
                (_, false) => "Right pane",
            };
            label(
                dc,
                inset(label_area, s(9), 0),
                heading,
                if active { p.accent } else { p.muted },
                DT_LEFT,
            );

            let path_frame = RECT {
                left: bounds.left + s(70),
                top: bounds.top + s(60),
                bottom: bounds.top + s(93),
                right: bounds.right - s(59),
            };
            background(dc, path_frame, p.edge);
            background(dc, inset(path_frame, 1, 1), p.surface);
            let filter = RECT {
                top: bounds.top + s(97),
                bottom: bounds.top + s(125),
                right: bounds.right - s(59),
                ..bounds
            };
            background(dc, filter, p.edge);
            background(dc, inset(filter, 1, 1), p.surface);
            label(
                dc,
                RECT {
                    left: filter.left + s(10),
                    right: filter.left + s(55),
                    ..filter
                },
                "Filter",
                p.muted,
                DT_LEFT,
            );
            let list_frame = RECT {
                top: bounds.top + s(135),
                ..bounds
            };
            background(dc, list_frame, p.edge);
            background(dc, inset(list_frame, 1, 1), p.surface);
            let status_area = RECT {
                top: bounds.bottom - s(30),
                ..bounds
            };
            background(dc, inset(status_area, 1, 1), p.strip);
            let pane = &self.panes[index];
            let listing = pane.listing.borrow();
            let shown = listing.rows.len();
            let total = listing.entries.len();
            let count = grouped(shown as u64);
            let selected = pane.count();
            let noun = if shown == 1 { "item" } else { "items" };
            let status = if let Some(error) = &listing.error {
                format!("Could not open folder · {error}")
            } else if listing.loading && !listing.complete && shown == 0 {
                "Loading folder…".to_owned()
            } else if listing.loading && !listing.complete {
                format!("{count} shown · Loading…")
            } else if listing.projecting {
                "Updating view…".to_owned()
            } else {
                let mut status = if listing.query.is_empty() {
                    format!("{count} {noun}")
                } else {
                    format!("{count} of {} items", grouped(total as u64))
                };
                if selected != 0 {
                    status.push_str(&format!(" · {} selected", grouped(selected as u64)));
                }
                if listing.watch == WatchState::Unavailable {
                    status.push_str(" · Ctrl+R to refresh");
                }
                status
            };
            label(dc, inset(status_area, s(10), 0), &status, p.muted, DT_LEFT);
        }

        let grip_x = (layout.gutter.left + layout.gutter.right) / 2;
        let grip_y = (layout.gutter.top + layout.gutter.bottom) / 2;
        for offset in [-5, 0, 5] {
            background(
                dc,
                RECT {
                    left: grip_x - s(1),
                    right: grip_x + s(1),
                    top: grip_y + s(offset),
                    bottom: grip_y + s(offset + 2),
                },
                p.edge,
            );
        }
        let note = self.operation_status.borrow();
        let compact = layout.footer.right - layout.footer.left < s(880);
        let area = RECT {
            left: layout.footer.left + s(if compact { 441 } else { 522 }),
            right: layout.footer.right - s(284),
            ..layout.footer
        };
        if area.right > area.left {
            label(dc, area, &note, p.muted, DT_RIGHT);
        }
    }

    fn paint_tabs(&self, strip: HWND, dc: HDC) {
        let Some((index, pane)) = self
            .panes
            .iter()
            .enumerate()
            .find(|(_, pane)| pane.tabs == strip)
        else {
            return;
        };
        let mut client = RECT::default();
        let p = self.palette.borrow();
        let tabs = pane.navigation.borrow();
        let previous_font = unsafe {
            let _ = GetClientRect(strip, &mut client);
            SelectObject(dc, self.font.get().into())
        };
        background(dc, client, p.canvas);
        let mut edge = client;
        edge.top = edge.bottom - 1;
        background(dc, edge, p.edge);
        for (position, tab) in tabs.items.iter().enumerate() {
            let mut rect = RECT::default();
            if send(
                strip,
                TCM_GETITEMRECT,
                position,
                (&mut rect as *mut RECT) as isize,
            ) == 0
                || rect.right <= client.left
                || rect.left >= client.right
            {
                continue;
            }
            rect.right -= self.scale(3);
            let selected = tabs.active == position;
            background(dc, rect, p.edge);
            let fill = if selected && self.active.get() == index {
                p.active_strip
            } else if selected {
                p.surface
            } else {
                p.strip
            };
            background(dc, inset(rect, 1, 1), fill);
            if selected {
                let line = RECT {
                    top: rect.bottom - self.scale(2),
                    ..rect
                };
                background(dc, line, p.accent);
            }
            let title = tab.location.title().to_string_lossy();
            label(
                dc,
                inset(rect, self.scale(9), 0),
                &title,
                if selected { p.text } else { p.muted },
                DT_LEFT,
            );
            if selected && unsafe { GetFocus() } == strip {
                unsafe {
                    let _ = DrawFocusRect(dc, &inset(rect, self.scale(3), self.scale(3)));
                }
            }
        }
        unsafe {
            SelectObject(dc, previous_font);
        }
    }

    pub fn draw_button(&self, item: &DRAWITEMSTRUCT, font: HFONT, mnemonic: bool) {
        let p = self.palette.borrow();
        let focused = item.itemState.0 & ODS_FOCUS.0 != 0;
        let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
        let disabled = item.itemState.0 & ODS_DISABLED.0 != 0;
        let fill = if pressed { p.active_strip } else { p.surface };
        let previous_font = unsafe { SelectObject(item.hDC, font.into()) };
        background(
            item.hDC,
            item.rcItem,
            if focused { p.accent } else { p.edge },
        );
        background(item.hDC, inset(item.rcItem, 1, 1), fill);
        let mut face = wide(window_text(item.hwndItem));
        face.pop();
        let prefix = if !mnemonic {
            DT_NOPREFIX
        } else if item.itemState.0 & ODS_NOACCEL.0 != 0 {
            DT_HIDEPREFIX
        } else {
            DRAW_TEXT_FORMAT(0)
        };
        unsafe {
            SetTextColor(item.hDC, if disabled { p.edge } else { p.text });
            SetBkMode(item.hDC, TRANSPARENT);
            DrawTextW(
                item.hDC,
                &mut face,
                &mut inset(item.rcItem, 4, 0),
                DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_CENTER | prefix,
            );
            if focused {
                let _ = DrawFocusRect(item.hDC, &inset(item.rcItem, 3, 3));
            }
            SelectObject(item.hDC, previous_font);
        }
    }

    pub(super) fn draw_header(&self, list: HWND, header: &NMCUSTOMDRAW) -> LRESULT {
        let p = self.palette.borrow();
        if p.contrast {
            return LRESULT(CDRF_DODEFAULT as isize);
        }
        match header.dwDrawStage {
            CDDS_PREPAINT => {
                // Paint the trailing part of the header beyond the last column as well.
                let mut rect = RECT::default();
                unsafe {
                    let _ = GetClientRect(header.hdr.hwndFrom, &mut rect);
                }
                background(header.hdc, rect, p.strip);
                LRESULT((CDRF_NOTIFYITEMDRAW | CDRF_NOTIFYPOSTPAINT) as isize)
            }
            CDDS_ITEMPREPAINT => {
                let rect = header.rc;
                background(header.hdc, rect, p.strip);
                background(
                    header.hdc,
                    RECT {
                        top: rect.bottom - 1,
                        ..rect
                    },
                    p.edge,
                );
                let (text, align) = match header.dwItemSpec {
                    0 => ("Name", DT_LEFT),
                    1 => ("Size", DT_RIGHT),
                    _ => ("Modified", DT_LEFT),
                };
                let sort = self
                    .panes
                    .iter()
                    .find(|pane| pane.list == list)
                    .map(|pane| pane.listing.borrow().sort)
                    .unwrap_or_default();
                let sorted = sort.column.index() == header.dwItemSpec;
                let text_rect = if sorted {
                    RECT {
                        right: rect.right - self.scale(13),
                        ..rect
                    }
                } else {
                    rect
                };
                label(
                    header.hdc,
                    inset(text_rect, self.scale(9), 0),
                    text,
                    p.muted,
                    align,
                );
                if sorted {
                    label(
                        header.hdc,
                        inset(rect, self.scale(5), 0),
                        if sort.descending { "▾" } else { "▴" },
                        p.accent,
                        DT_RIGHT,
                    );
                }
                LRESULT(CDRF_SKIPDEFAULT as isize)
            }
            CDDS_POSTPAINT => {
                let mut rect = RECT::default();
                let mut last = RECT::default();
                unsafe {
                    let _ = GetClientRect(header.hdr.hwndFrom, &mut rect);
                }
                if send(
                    header.hdr.hwndFrom,
                    HDM_GETITEMRECT,
                    2,
                    (&mut last as *mut RECT) as isize,
                ) != 0
                {
                    rect.left = last.right;
                    background(header.hdc, rect, p.strip);
                    background(
                        header.hdc,
                        RECT {
                            top: rect.bottom - 1,
                            ..rect
                        },
                        p.edge,
                    );
                }
                LRESULT(CDRF_DODEFAULT as isize)
            }
            _ => LRESULT(CDRF_DODEFAULT as isize),
        }
    }
}

fn place(control: HWND, x: i32, y: i32, width: i32, height: i32) {
    unsafe {
        let _ = SetWindowPos(
            control,
            None,
            x,
            y,
            width.max(0),
            height.max(0),
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

fn inset(rect: RECT, x: i32, y: i32) -> RECT {
    RECT {
        left: rect.left + x,
        right: rect.right - x,
        top: rect.top + y,
        bottom: rect.bottom - y,
    }
}

fn background(dc: HDC, rect: RECT, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        FillRect(dc, &rect, brush);
        let _ = DeleteObject(brush.into());
    }
}

fn label(dc: HDC, mut rect: RECT, text: &str, color: COLORREF, align: DRAW_TEXT_FORMAT) {
    unsafe {
        SetTextColor(dc, color);
        SetBkMode(dc, TRANSPARENT);
        let mut text = wide(text);
        text.pop();
        DrawTextW(
            dc,
            &mut text,
            &mut rect,
            DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX | align,
        );
    }
}

pub(super) unsafe extern "system" fn tab_proc(
    window: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    parent: usize,
) -> LRESULT {
    unsafe {
        let app = (GetWindowLongPtrW(HWND(parent as _), GWLP_USERDATA) as *const App).as_ref();
        if let Some(app) = app {
            match msg {
                WM_ERASEBKGND => return LRESULT(1),
                WM_PAINT => {
                    let mut paint = PAINTSTRUCT::default();
                    let dc = BeginPaint(window, &mut paint);
                    app.paint_tabs(window, dc);
                    let _ = EndPaint(window, &paint);
                    return LRESULT(0);
                }
                WM_PRINTCLIENT => {
                    app.paint_tabs(window, HDC(wparam.0 as _));
                    return LRESULT(0);
                }
                _ => {}
            }
        }
        if msg == WM_NCDESTROY {
            let _ = RemoveWindowSubclass(window, Some(tab_proc), id);
        }
        DefSubclassProc(window, msg, wparam, lparam)
    }
}
