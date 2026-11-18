use std::{
    collections::VecDeque,
    ffi::OsStr,
    path::{Component, Prefix},
};

use super::*;

pub const MAX_TABS: usize = 32;
pub const MAX_PAIRS: usize = 32;
const HISTORY_LIMIT: usize = 32;

#[derive(Clone, Default)]
pub struct Anchors {
    pub top: Vec<u16>,
    pub focused: Vec<u16>,
    pub selected: Vec<u16>,
    pub top_index: usize,
}

impl Anchors {
    pub fn is_empty(&self) -> bool {
        self.top.is_empty()
            && self.focused.is_empty()
            && self.selected.is_empty()
            && self.top_index == 0
    }
}

#[derive(Clone)]
pub struct Location {
    pub path: PathBuf,
    pub archive: Option<Arc<str>>,
    pub query: Vec<u16>,
    pub anchors: Anchors,
}

impl Location {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            archive: None,
            query: Vec::new(),
            anchors: Anchors::default(),
        }
    }

    pub fn archive(path: PathBuf, folder: Arc<str>) -> Self {
        Self {
            archive: Some(folder),
            ..Self::new(path)
        }
    }

    pub fn title(&self) -> &OsStr {
        if let Some(folder) = &self.archive
            && !folder.is_empty()
        {
            return OsStr::new(folder.rsplit('/').next().unwrap());
        }
        if let Some(name) = self.path.file_name() {
            return name;
        }
        if let Some(Component::Prefix(prefix)) = self.path.components().next()
            && let Prefix::UNC(_, share) | Prefix::VerbatimUNC(_, share) = prefix.kind()
        {
            return share;
        }
        self.path.as_os_str()
    }
}

#[derive(Clone)]
pub struct Tab {
    pub location: Location,
    pub sort: Sort,
    back: VecDeque<Location>,
    forward: Vec<Location>,
}

impl Tab {
    pub fn new(location: Location, sort: Sort) -> Self {
        Self {
            location,
            sort,
            back: VecDeque::new(),
            forward: Vec::new(),
        }
    }

    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    fn push_back(&mut self, location: Location) {
        if self.back.len() == HISTORY_LIMIT {
            self.back.pop_front();
        }
        self.back.push_back(location);
    }

    pub fn go(&mut self, location: Location) {
        let previous = std::mem::replace(&mut self.location, location);
        self.push_back(previous);
        self.forward.clear();
    }

    pub fn travel(&mut self, forward: bool) -> bool {
        if forward {
            if let Some(next) = self.forward.pop() {
                let current = std::mem::replace(&mut self.location, next);
                self.push_back(current);
                return true;
            }
        } else if let Some(previous) = self.back.pop_back() {
            self.forward
                .push(std::mem::replace(&mut self.location, previous));
            return true;
        }
        false
    }

    fn snapshot(&self) -> Self {
        Self::new(self.location.clone(), self.sort)
    }
}

#[derive(Clone)]
pub struct Tabs {
    pub items: Vec<Tab>,
    pub active: usize,
}

impl Tabs {
    pub fn single(path: PathBuf, sort: Sort) -> Self {
        Self {
            items: vec![Tab::new(Location::new(path), sort)],
            active: 0,
        }
    }

    pub fn current(&self) -> &Tab {
        &self.items[self.active]
    }

    pub fn current_mut(&mut self) -> &mut Tab {
        &mut self.items[self.active]
    }

    pub fn open(&mut self) -> bool {
        self.insert(self.current().snapshot())
    }

    pub fn open_location(&mut self, location: Location) -> bool {
        self.insert(Tab::new(location, self.current().sort))
    }

    fn insert(&mut self, tab: Tab) -> bool {
        if self.items.len() == MAX_TABS {
            return false;
        }
        self.active += 1;
        self.items.insert(self.active, tab);
        true
    }

    pub fn close(&mut self) -> bool {
        if self.items.len() == 1 {
            return false;
        }
        self.items.remove(self.active);
        self.active = self.active.min(self.items.len() - 1);
        true
    }

    pub fn snapshot(&self) -> Self {
        Self {
            items: self.items.iter().map(Tab::snapshot).collect(),
            active: self.active,
        }
    }
}

#[derive(Clone)]
pub struct SavedPair {
    pub name: OsString,
    pub paths: [PathBuf; 2],
}

impl App {
    pub fn capture_tab(&self, index: usize) {
        let pane = &self.panes[index];
        let listing = pane.listing.borrow();
        if listing.path.as_os_str().is_empty() {
            return;
        }
        let anchors = listing.restore.clone().unwrap_or_else(|| {
            let top = send(pane.list, LVM_GETTOPINDEX, 0, 0).max(0) as usize;
            let focused = send(
                pane.list,
                LVM_GETNEXTITEM,
                usize::MAX,
                LVNI_FOCUSED as isize,
            );
            let name_at = |row| {
                listing.rows.get(row).map_or_else(Vec::new, |entry| {
                    entry.name[..entry.name.len() - 1].to_vec()
                })
            };
            let selected = if pane.count() == 1 {
                pane.selection().next().map_or_else(Vec::new, name_at)
            } else {
                // A bulk selection can be large. Return to its focused row without selecting a subset.
                Vec::new()
            };
            Anchors {
                top: name_at(top),
                focused: name_at(focused as usize),
                selected,
                top_index: top,
            }
        });
        let mut tabs = pane.navigation.borrow_mut();
        let tab = tabs.current_mut();
        tab.location.anchors = anchors;
        tab.location.query.clone_from(&listing.query);
        tab.sort = listing.sort;
    }

    pub fn activate_tab(&self, index: usize) {
        let (location, sort) = {
            let tabs = self.panes[index].navigation.borrow();
            (tabs.current().location.clone(), tabs.current().sort)
        };
        self.update_tab(index);
        self.show_location(index, location, sort);
        self.update_sort_header(index);
    }

    pub fn select_tab(&self, index: usize, target: usize) {
        let pane = &self.panes[index];
        {
            let tabs = pane.navigation.borrow();
            if target >= tabs.items.len() {
                return;
            }
            if target == tabs.active {
                drop(tabs);
                self.focus_list(index);
                return;
            }
        }
        self.capture_tab(index);
        pane.navigation.borrow_mut().active = target;
        self.activate_tab(index);
        self.focus_list(index);
    }

    pub fn cycle_tab(&self, index: usize, previous: bool) {
        let target = {
            let tabs = self.panes[index].navigation.borrow();
            let count = tabs.items.len();
            (tabs.active + if previous { count - 1 } else { 1 }) % count
        };
        self.select_tab(index, target);
    }

    pub fn create_tab(&self, index: usize) {
        self.capture_tab(index);
        if self.panes[index].navigation.borrow_mut().open() {
            self.rebuild_tabs(index);
            self.activate_tab(index);
        }
        self.focus_list(index);
    }

    pub fn close_tab(&self, index: usize) {
        if self.panes[index].navigation.borrow_mut().close() {
            self.rebuild_tabs(index);
            self.activate_tab(index);
        }
        self.focus_list(index);
    }

    pub fn history(&self, index: usize, forward: bool) {
        self.capture_tab(index);
        if self.panes[index]
            .navigation
            .borrow_mut()
            .current_mut()
            .travel(forward)
        {
            self.activate_tab(index);
        }
        self.focus_list(index);
    }

    fn tab_text(&self, index: usize, tab: usize) -> Vec<u16> {
        let tabs = self.panes[index].navigation.borrow();
        wide(tabs.items[tab].location.title())
    }

    pub fn rebuild_tabs(&self, index: usize) {
        let pane = &self.panes[index];
        let count = pane.navigation.borrow().items.len();
        send(pane.tabs, TCM_DELETEALLITEMS, 0, 0);
        for tab in 0..count {
            let mut text = self.tab_text(index, tab);
            let item = TCITEMW {
                mask: TCIF_TEXT,
                pszText: PWSTR(text.as_mut_ptr()),
                ..Default::default()
            };
            send(
                pane.tabs,
                TCM_INSERTITEMW,
                tab,
                (&item as *const TCITEMW) as isize,
            );
        }
        let active = pane.navigation.borrow().active;
        send(pane.tabs, TCM_SETCURSEL, active, 0);
    }

    fn update_tab(&self, index: usize) {
        let pane = &self.panes[index];
        let (active, count, can_back, can_forward) = {
            let tabs = pane.navigation.borrow();
            (
                tabs.active,
                tabs.items.len(),
                tabs.current().can_back(),
                tabs.current().can_forward(),
            )
        };
        let mut text = self.tab_text(index, active);
        let item = TCITEMW {
            mask: TCIF_TEXT,
            pszText: PWSTR(text.as_mut_ptr()),
            ..Default::default()
        };
        send(
            pane.tabs,
            TCM_SETITEMW,
            active,
            (&item as *const TCITEMW) as isize,
        );
        send(pane.tabs, TCM_SETCURSEL, active, 0);
        unsafe {
            let _ = EnableWindow(pane.new_tab, count < MAX_TABS);
            let _ = EnableWindow(pane.close_tab, count > 1);
            let _ = EnableWindow(pane.back, can_back);
            let _ = EnableWindow(pane.forward, can_forward);
            let _ = InvalidateRect(Some(pane.tabs), None, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backs_and_forwards_within_each_tab_and_discards_forward_on_new_navigation() {
        let a = PathBuf::from(r"C:\A");
        let b = PathBuf::from(r"C:\B");
        let c = PathBuf::from(r"C:\C");
        let mut tabs = Tabs::single(a.clone(), Sort::default());
        tabs.current_mut().go(Location::new(b.clone()));
        assert!(tabs.open());
        assert!(!tabs.current_mut().travel(false));
        tabs.current_mut().go(Location::new(c.clone()));
        tabs.active = 0;
        assert!(tabs.current_mut().travel(false));
        assert_eq!(tabs.current().location.path, a);
        assert!(tabs.current_mut().travel(true));
        assert_eq!(tabs.current().location.path, b);
        assert!(tabs.current_mut().travel(false));
        tabs.current_mut().go(Location::new(c.clone()));
        assert!(!tabs.current_mut().travel(true));
        assert!(tabs.close());
        assert_eq!(tabs.current().location.path, c);
        assert!(!tabs.close());
    }

    #[test]
    fn uses_the_share_name_for_unc_root_tabs() {
        for root in [r"\\server\share", r"\\?\UNC\server\share\"] {
            assert_eq!(Location::new(PathBuf::from(root)).title(), "share");
        }
        assert_eq!(Location::new(PathBuf::from(r"C:\")).title(), r"C:\");
    }
}
