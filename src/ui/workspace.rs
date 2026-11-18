use super::*;

use navigation::{MAX_PAIRS, MAX_TABS, SavedPair, Tab};

const KEY: PCWSTR = w!("Software\\DualPane");
const VALUE: PCWSTR = w!("Workspace");

pub struct Workspace {
    pub panes: [Tabs; 2],
    pub pairs: Vec<SavedPair>,
    pub split: f32,
    pub theme: Theme,
    pub persist: bool,
}

impl Default for Workspace {
    fn default() -> Self {
        let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from(r"C:\"));
        let home = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| current.clone());
        Self {
            panes: [home, current].map(|path| Tabs::single(path, Sort::default())),
            pairs: Vec::new(),
            split: 0.5,
            theme: Theme::System,
            persist: true,
        }
    }
}

impl Workspace {
    pub fn startup() -> Self {
        let mut paths = Vec::new();
        let mut persist = true;
        let mut options = true;
        for arg in std::env::args_os().skip(1) {
            if options && arg == "--" {
                options = false;
            } else if options && arg == "--no-workspace" {
                persist = false;
            } else {
                paths.push(PathBuf::from(arg));
            }
        }
        let mut workspace = if persist {
            Self::load().unwrap_or_default()
        } else {
            Self::default()
        };
        workspace.persist = persist;
        for (pane, override_path) in workspace.panes.iter_mut().zip(paths) {
            if let Ok(path) = std::path::absolute(override_path) {
                pane.current_mut().location = Location::new(path);
            }
        }
        workspace
    }

    fn load() -> Option<Self> {
        let mut size = 0;
        unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                KEY,
                VALUE,
                RRF_RT_REG_MULTI_SZ,
                None,
                None,
                Some(&mut size),
            )
            .ok()
            .ok()?;
        }
        if size == 0 || size > 9 * 1024 * 1024 || size % 2 != 0 {
            return None;
        }
        let mut data = vec![0_u16; size as usize / 2];
        unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                KEY,
                VALUE,
                RRF_RT_REG_MULTI_SZ,
                None,
                Some(data.as_mut_ptr().cast()),
                Some(&mut size),
            )
            .ok()
            .ok()?;
        }
        Self::decode(&data[..size as usize / 2])
    }

    pub fn save(&self) -> Result<()> {
        let data = self.encode();
        let bytes = unsafe { std::slice::from_raw_parts(data.as_ptr().cast(), data.len() * 2) };
        let mut key = HKEY::default();
        unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                KEY,
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            )
            .ok()?;
            let result = RegSetValueExW(key, VALUE, None, REG_MULTI_SZ, Some(bytes)).ok();
            let _ = RegCloseKey(key);
            result
        }
    }

    fn encode(&self) -> Vec<u16> {
        let theme = match self.theme {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        };
        let mut data = wide("v2");
        data.extend(wide(theme));
        data.extend(wide(self.split.to_string()));
        // REG_MULTI_SZ uses an empty string as its terminator. Prefix each
        // record field so empty filters and missing row anchors round trip.
        let mut write = |field: &[u16]| {
            data.push(b'+' as u16);
            data.extend_from_slice(field);
            data.push(0);
        };
        for pane in &self.panes {
            write(&digits(pane.items.len()));
            write(&digits(pane.active));
            for tab in &pane.items {
                let sort = tab.sort.column.index() * 2 + usize::from(tab.sort.descending);
                write(&digits(sort));
                let place = &tab.location;
                write(&units(&place.path));
                write(&place.query);
                write(&digits(place.anchors.top_index));
                write(&place.anchors.top);
                write(&place.anchors.focused);
                write(&place.anchors.selected);
            }
        }
        write(&digits(self.pairs.len()));
        for pair in &self.pairs {
            write(&units(&pair.name));
            for folder in &pair.paths {
                write(&units(folder));
            }
        }
        data.push(0);
        data
    }

    fn decode(data: &[u16]) -> Option<Self> {
        let mut parts = data.split(|&unit| unit == 0);
        let version = String::from_utf16(parts.next()?).ok()?;
        let theme = match String::from_utf16(parts.next()?).ok()?.as_str() {
            "system" => Theme::System,
            "light" => Theme::Light,
            "dark" => Theme::Dark,
            _ => return None,
        };
        let split = String::from_utf16(parts.next()?)
            .ok()?
            .parse::<f32>()
            .ok()?;
        if !split.is_finite() {
            return None;
        }
        let (panes, pairs) = match version.as_str() {
            "v1" => {
                let left = decode_sort(parts.next()?)?;
                let right = decode_sort(parts.next()?)?;
                (
                    [
                        Tabs::single(folder(parts.next()?)?, left),
                        Tabs::single(folder(parts.next()?)?, right),
                    ],
                    Vec::new(),
                )
            }
            "v2" => {
                let mut read = || parts.next()?.strip_prefix(&[b'+' as u16]);
                let panes = [decode_tabs(&mut read)?, decode_tabs(&mut read)?];
                let count = number(read()?)?;
                if count > MAX_PAIRS {
                    return None;
                }
                let mut pairs = Vec::with_capacity(count);
                for _ in 0..count {
                    let name = read()?;
                    if name.is_empty() || name.len() > 255 {
                        return None;
                    }
                    pairs.push(SavedPair {
                        name: OsString::from_wide(name),
                        paths: [folder(read()?)?, folder(read()?)?],
                    });
                }
                (panes, pairs)
            }
            _ => return None,
        };
        Some(Self {
            panes,
            pairs,
            split: split.clamp(0.30, 0.70),
            theme,
            persist: true,
        })
    }
}

fn units(value: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().collect()
}

fn digits(value: usize) -> Vec<u16> {
    value.to_string().encode_utf16().collect()
}

fn number(value: &[u16]) -> Option<usize> {
    String::from_utf16(value).ok()?.parse().ok()
}

fn folder(value: &[u16]) -> Option<PathBuf> {
    let path = PathBuf::from(OsString::from_wide(value));
    (value.len() <= 32767 && path.is_absolute()).then_some(path)
}

fn decode_sort(value: &[u16]) -> Option<Sort> {
    let code = number(value)?;
    Some(Sort {
        column: Column::from_index(code / 2)?,
        descending: !code.is_multiple_of(2),
    })
}

fn decode_tabs<'a>(read: &mut impl FnMut() -> Option<&'a [u16]>) -> Option<Tabs> {
    let count = number(read()?)?;
    let active = number(read()?)?;
    if count > MAX_TABS || active >= count {
        return None;
    }
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        let sort = decode_sort(read()?)?;
        let path = folder(read()?)?;
        let query = read()?;
        let top_index = number(read()?)?;
        let [top, focused, selected] = [read()?, read()?, read()?];
        if [query, top, focused, selected]
            .into_iter()
            .any(|field| field.len() > 255)
        {
            return None;
        }
        let anchors = Anchors {
            top_index,
            top: top.to_vec(),
            focused: focused.to_vec(),
            selected: selected.to_vec(),
        };
        items.push(Tab::new(
            Location {
                path,
                query: query.to_vec(),
                anchors,
            },
            sort,
        ));
    }
    Some(Tabs { items, active })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_milestone_two_workspace_to_one_tab_per_pane() {
        let old = [
            "v1",
            "dark",
            "0.615",
            "0",
            "5",
            r"C:\År",
            r"\\server\share\日本語",
        ];
        let data = old
            .into_iter()
            .flat_map(wide)
            .chain([0])
            .collect::<Vec<_>>();
        let saved = Workspace::decode(&data).unwrap();
        assert_eq!(saved.panes[0].current().location.path, Path::new(r"C:\År"));
        assert_eq!(
            saved.panes[1].current().location.path,
            Path::new(r"\\server\share\日本語")
        );
        assert!(saved.panes[1].current().sort.descending);
        assert_eq!(saved.split, 0.615);
        assert!(matches!(saved.theme, Theme::Dark));
        assert!(Workspace::decode(&data[..5]).is_none());
    }

    #[test]
    fn restores_active_tabs_and_saved_pairs_with_unicode_and_empty_fields() {
        let mut workspace = Workspace::default();
        let folder = PathBuf::from(r"C:\År");
        workspace.panes[0] = Tabs::single(folder.clone(), Sort::default());
        assert!(workspace.panes[0].open());
        let tab = workspace.panes[0].current_mut();
        tab.location.path = PathBuf::from(r"C:\日本語");
        tab.location.query = units("æ");
        tab.location.anchors.top = units("æ.txt");
        tab.location.anchors.top_index = 8;
        workspace.pairs.push(SavedPair {
            name: OsString::from("Å → 日本語"),
            paths: [folder, tab.location.path.clone()],
        });
        let data = workspace.encode();
        let saved = Workspace::decode(&data).unwrap();
        assert_eq!(saved.panes[0].items.len(), 2);
        assert_eq!(saved.panes[0].active, 1);
        assert_eq!(saved.panes[0].current().location.query, units("æ"));
        assert_eq!(
            saved.panes[0].current().location.anchors.top,
            units("æ.txt")
        );
        assert_eq!(saved.panes[0].current().location.anchors.top_index, 8);
        assert!(saved.panes[0].items[0].location.query.is_empty());
        assert_eq!(saved.pairs[0].name, "Å → 日本語");
        assert_eq!(saved.pairs[0].paths, workspace.pairs[0].paths);
    }
}
