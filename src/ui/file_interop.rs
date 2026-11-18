use std::rc::Rc;

use windows::{
    Win32::System::{
        Com::IDataObject,
        DataExchange::{
            AddClipboardFormatListener, GetClipboardSequenceNumber, RemoveClipboardFormatListener,
        },
        Ole::*,
        SystemServices::{MK_CONTROL, MK_LBUTTON, MK_SHIFT, MODIFIERKEYS_FLAGS},
    },
    core::{BOOL, HRESULT, Ref, implement},
};

use super::*;
use crate::{file_data, transfer::Request};

struct Cut {
    sequence: u32,
    folder: PathBuf,
    names: HashSet<Vec<u16>>,
}

#[derive(Default)]
pub(super) struct FileInterop {
    cut: RefCell<Option<Cut>>,
    target_app: Rc<Cell<*const App>>,
    registered: [Cell<bool>; 2],
    listener: Cell<bool>,
    drag_from: RefCell<Option<PathBuf>>,
    pub drop_hint: Cell<Option<(usize, &'static str)>>,
}

impl App {
    pub fn register_file_interop(&self) -> Result<()> {
        let interop = &self.file_interop;
        unsafe {
            AddClipboardFormatListener(self.window)?;
        }
        interop.listener.set(true);
        interop.target_app.set(self as *const App);
        for (index, pane) in self.panes.iter().enumerate() {
            send(pane.list, LVM_SETCALLBACKMASK, LVIS_CUT.0 as usize, 0);
            let target: IDropTarget = PaneTarget {
                app: interop.target_app.clone(),
                pane: index,
                offered: Cell::new(false),
                allowed: Cell::new(DROPEFFECT_NONE),
            }
            .into();
            unsafe {
                RegisterDragDrop(pane.list, &target)?;
            }
            interop.registered[index].set(true);
        }
        Ok(())
    }

    pub fn revoke_file_interop(&self) {
        let interop = &self.file_interop;
        interop.target_app.set(std::ptr::null());
        for (index, pane) in self.panes.iter().enumerate() {
            if interop.registered[index].replace(false) {
                unsafe {
                    let _ = RevokeDragDrop(pane.list);
                }
            }
        }
        if interop.listener.replace(false) {
            unsafe {
                let _ = RemoveClipboardFormatListener(self.window);
            }
        }
    }

    fn redraw_cut_items(&self) {
        for pane in &self.panes {
            unsafe {
                let _ = InvalidateRect(Some(pane.list), None, false);
            }
        }
    }

    pub fn clipboard_changed(&self) {
        let mut cut = self.file_interop.cut.borrow_mut();
        if cut
            .as_ref()
            .is_some_and(|cut| cut.sequence != unsafe { GetClipboardSequenceNumber() })
        {
            cut.take();
            drop(cut);
            self.redraw_cut_items();
        }
    }

    pub fn row_is_cut(&self, listing: &Listing, name: &[u16]) -> bool {
        listing.archive.is_none()
            && self
                .file_interop
                .cut
                .borrow()
                .as_ref()
                .is_some_and(|cut| cut.folder == listing.path && cut.names.contains(name))
    }

    pub fn publish_files(&self, cut: bool) {
        let pane = &self.panes[self.active.get()];
        if !pane.listing.borrow().disk_ready() {
            return;
        }
        let (folder, files) = pane.selected_paths();
        if files.is_empty() {
            return;
        }
        match file_data::clipboard_write(self.window, &files, cut) {
            Ok(sequence) => {
                self.file_interop.cut.replace(cut.then(|| {
                    Cut {
                        sequence,
                        folder,
                        names: files
                            .iter()
                            .filter_map(|path| path.file_name().map(wide))
                            .collect(),
                    }
                }));
                self.redraw_cut_items();
                *self.operation_status.borrow_mut() =
                    format!("{} on the clipboard", selection_label(files.len()));
                self.status_changed();
            }
            Err(error) => {
                message(
                    self.window,
                    "Could not copy file paths",
                    &error.message(),
                    MB_OK | MB_ICONWARNING,
                );
            }
        }
    }

    pub fn paste_files(&self) {
        let index = self.active.get();
        let Some(destination) = self.file_destination(index) else {
            return;
        };
        match file_data::clipboard_read(self.window) {
            Ok((paths, cut, sequence)) => {
                let sequence = cut.then_some(sequence);
                if sequence.is_some_and(|sequence| self.clipboard_move_pending(sequence)) {
                    *self.operation_status.borrow_mut() =
                        "This clipboard move is already queued.".into();
                    self.status_changed();
                    return;
                }
                self.queue_incoming(
                    destination,
                    paths,
                    if cut { Transfer::Move } else { Transfer::Copy },
                    sequence,
                );
            }
            Err(error) => {
                message(
                    self.window,
                    "Could not paste files",
                    &error.message(),
                    MB_OK | MB_ICONINFORMATION,
                );
            }
        }
    }

    pub fn finish_clipboard_move(&self, sequence: u32) {
        file_data::clear_if_current(self.window, sequence);
        self.clipboard_changed();
    }

    fn file_destination(&self, pane: usize) -> Option<PathBuf> {
        let listing = self.panes[pane].listing.borrow();
        listing.disk_ready().then(|| listing.path.clone())
    }

    fn queue_incoming(
        &self,
        destination: PathBuf,
        paths: Vec<PathBuf>,
        action: Transfer,
        sequence: Option<u32>,
    ) -> bool {
        if !self.can_queue_transfer() {
            *self.operation_status.borrow_mut() = "Transfer queue full".into();
            self.status_changed();
            return false;
        }
        let Some(request) = Request::incoming(action, destination, paths, sequence) else {
            return false;
        };
        self.enqueue_transfer(request);
        true
    }

    fn drop_action(
        &self,
        pane: usize,
        offered: bool,
        allowed: DROPEFFECT,
        keys: MODIFIERKEYS_FLAGS,
    ) -> DROPEFFECT {
        if !offered || !self.can_queue_transfer() {
            return DROPEFFECT_NONE;
        }
        let listing = self.panes[pane].listing.borrow();
        if !listing.disk_ready()
            || self.file_interop.drag_from.borrow().as_ref() == Some(&listing.path)
        {
            return DROPEFFECT_NONE;
        }
        choose_effect(allowed, keys)
    }

    fn highlight_drop(&self, panel: usize, effect: DROPEFFECT) {
        let next = if effect == DROPEFFECT_MOVE {
            Some((panel, "Drop here · Move"))
        } else if effect == DROPEFFECT_COPY {
            Some((panel, "Drop here · Copy"))
        } else {
            None
        };
        let prior = self.file_interop.drop_hint.replace(next);
        if prior == next {
            return;
        }
        let layout = self.layout.get();
        for entry in [prior, next].into_iter().flatten() {
            unsafe {
                let _ = InvalidateRect(Some(self.window), Some(&layout.panes[entry.0]), false);
            }
        }
    }

    pub fn start_file_drag(&self, index: usize) {
        let pane = &self.panes[index];
        if !pane.listing.borrow().disk_ready() || self.file_interop.drag_from.borrow().is_some() {
            return;
        }
        let (folder, paths) = pane.selected_paths();
        if paths.is_empty() {
            return;
        }
        match file_data::drag_data(&paths) {
            Ok(data) => {
                let source: IDropSource = DragSource.into();
                self.file_interop.drag_from.replace(Some(folder));
                let mut effect = DROPEFFECT_NONE;
                unsafe {
                    let _ = DoDragDrop(
                        &data,
                        &source,
                        DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0),
                        &mut effect,
                    );
                }
                self.file_interop.drag_from.take();
                self.highlight_drop(index, DROPEFFECT_NONE);
            }
            Err(error) => {
                message(
                    self.window,
                    "Could not drag files",
                    &error.message(),
                    MB_OK | MB_ICONWARNING,
                );
            }
        }
    }
}

fn choose_effect(allowed: DROPEFFECT, keys: MODIFIERKEYS_FLAGS) -> DROPEFFECT {
    let shift = keys.0 & MK_SHIFT.0 != 0;
    let control = keys.0 & MK_CONTROL.0 != 0;
    let effect = match (control, shift) {
        (true, true) => return DROPEFFECT_NONE,
        (false, true) => DROPEFFECT_MOVE,
        (true, false) => DROPEFFECT_COPY,
        (false, false) if allowed.0 & DROPEFFECT_COPY.0 != 0 => DROPEFFECT_COPY,
        (false, false) => DROPEFFECT_MOVE,
    };
    DROPEFFECT(effect.0 & allowed.0)
}

#[implement(IDropSource)]
struct DragSource;

impl IDropSource_Impl for DragSource_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool() {
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

#[implement(IDropTarget)]
struct PaneTarget {
    app: Rc<Cell<*const App>>,
    pane: usize,
    offered: Cell<bool>,
    allowed: Cell<DROPEFFECT>,
}

impl PaneTarget_Impl {
    fn feedback(&self, keys: MODIFIERKEYS_FLAGS) -> DROPEFFECT {
        let Some(app) = (unsafe { self.app.get().as_ref() }) else {
            return DROPEFFECT_NONE;
        };
        app.drop_action(self.pane, self.offered.get(), self.allowed.get(), keys)
    }
}

impl IDropTarget_Impl for PaneTarget_Impl {
    fn DragEnter(
        &self,
        data: Ref<IDataObject>,
        keys: MODIFIERKEYS_FLAGS,
        point: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        let effect = unsafe { effect.as_mut() }.ok_or(E_POINTER)?;
        self.allowed.set(*effect);
        self.offered
            .set(data.as_ref().is_some_and(file_data::has_files));
        self.DragOver(keys, point, effect)
    }

    fn DragOver(
        &self,
        keys: MODIFIERKEYS_FLAGS,
        _: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        let effect = unsafe { effect.as_mut() }.ok_or(E_POINTER)?;
        *effect = self.feedback(keys);
        if let Some(app) = unsafe { self.app.get().as_ref() } {
            app.highlight_drop(self.pane, *effect);
        }
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        self.offered.set(false);
        if let Some(app) = unsafe { self.app.get().as_ref() } {
            app.highlight_drop(self.pane, DROPEFFECT_NONE);
        }
        Ok(())
    }

    fn Drop(
        &self,
        data: Ref<IDataObject>,
        keys: MODIFIERKEYS_FLAGS,
        _: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        let effect = unsafe { effect.as_mut() }.ok_or(E_POINTER)?;
        let selected = self.feedback(keys);
        *effect = DROPEFFECT_NONE;
        self.DragLeave()?;
        if selected == DROPEFFECT_NONE {
            return Ok(());
        }
        if let Some(app) = unsafe { self.app.get().as_ref() }
            && let Some(destination) = app.file_destination(self.pane)
            && let Some(data) = data.as_ref()
        {
            let files = file_data::drop_files(data)?;
            let action = if selected == DROPEFFECT_MOVE {
                Transfer::Move
            } else {
                Transfer::Copy
            };
            if app.queue_incoming(destination, files, action, None) {
                // A queued move is owned by our worker. The source must never
                // delete originals based on the immediate OLE return value.
                if selected == DROPEFFECT_MOVE {
                    file_data::no_source_deletion(data);
                } else {
                    *effect = DROPEFFECT_COPY;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn drop_modifiers_only_request_supported_operations() {
        let both = DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0);
        assert_eq!(choose_effect(both, MODIFIERKEYS_FLAGS(0)), DROPEFFECT_COPY);
        assert_eq!(choose_effect(both, MK_SHIFT), DROPEFFECT_MOVE);
        assert_eq!(choose_effect(DROPEFFECT_COPY, MK_SHIFT), DROPEFFECT_NONE);
        assert_eq!(
            choose_effect(both, MODIFIERKEYS_FLAGS(MK_CONTROL.0 | MK_SHIFT.0)),
            DROPEFFECT_NONE
        );
    }

    #[test]
    fn ole_drop_queues_a_move_and_does_not_request_source_deletion() -> Result<()> {
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let path = std::env::temp_dir().join(format!(
            "dual-pane-ole-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path)?;
        let fixture = Fixture(path);
        let destination = fixture.0.join("destination");
        fs::create_dir(&destination)?;
        let source = fixture.0.join("move.txt");
        fs::write(&source, b"Ole drop")?;

        let _ole = file_data::Ole::enter()?;
        let window = create_window()?;
        let app = attach(window, &Workspace::default())?;
        app.panes[1].listing.replace(Listing {
            path: destination.clone(),
            complete: true,
            ..Listing::default()
        });
        let target: IDropTarget = PaneTarget {
            app: Rc::new(Cell::new(&*app)),
            pane: 1,
            offered: Cell::new(false),
            allowed: Cell::new(DROPEFFECT_NONE),
        }
        .into();
        let data = file_data::drag_data(std::slice::from_ref(&source))?;
        let mut effect = DROPEFFECT(DROPEFFECT_MOVE.0 | DROPEFFECT_COPY.0);
        let point = POINTL::default();
        unsafe {
            target.DragEnter(&data, MK_SHIFT, point, &mut effect)?;
        }
        assert_eq!(effect, DROPEFFECT_MOVE);
        assert_eq!(
            app.file_interop.drop_hint.get(),
            Some((1, "Drop here · Move"))
        );
        unsafe {
            target.Drop(&data, MK_SHIFT, point, &mut effect)?;
        }
        assert_eq!(effect, DROPEFFECT_NONE);
        assert_eq!(app.file_interop.drop_hint.get(), None);
        let end = Instant::now() + Duration::from_secs(5);
        while app.operation_active.get() && Instant::now() < end {
            app.events();
            thread::sleep(Duration::from_millis(5));
        }
        unsafe {
            DestroyWindow(window)?;
        }
        assert!(!app.operation_active.get());
        assert!(!source.exists());
        assert_eq!(fs::read(destination.join("move.txt"))?, b"Ole drop");
        Ok(())
    }
}
