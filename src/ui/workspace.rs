use super::*;

use crate::directory::{Column, Sort};

const KEY: PCWSTR = w!("Software\\DualPane");
const VALUE: PCWSTR = w!("Workspace");

pub struct Workspace {
    pub paths: [PathBuf; 2],
    pub sort: [Sort; 2],
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
            paths: [home, current],
            sort: [Sort::default(); 2],
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
        for (path, override_path) in workspace.paths.iter_mut().zip(paths) {
            *path = override_path;
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
        if size == 0 || size > 140_000 || size % 2 != 0 {
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
        let position = self.split.to_string();
        let sort_code =
            |sort: Sort| (sort.column.index() * 2 + usize::from(sort.descending)).to_string();
        let mut data = wide("v1");
        for entry in [
            wide(theme),
            wide(position),
            wide(sort_code(self.sort[0])),
            wide(sort_code(self.sort[1])),
            wide(&self.paths[0]),
            wide(&self.paths[1]),
        ] {
            data.extend(entry);
        }
        data.push(0);
        data
    }

    fn decode(data: &[u16]) -> Option<Self> {
        let mut parts = data.split(|&unit| unit == 0);
        if parts.next()? != [b'v' as u16, b'1' as u16] {
            return None;
        }
        let theme = match String::from_utf16(parts.next()?).ok()?.as_str() {
            "system" => Theme::System,
            "light" => Theme::Light,
            "dark" => Theme::Dark,
            _ => return None,
        };
        let parse = |part: &[u16]| String::from_utf16(part).ok()?.parse::<usize>().ok();
        let position = String::from_utf16(parts.next()?)
            .ok()?
            .parse::<f32>()
            .ok()?;
        if !position.is_finite() {
            return None;
        }
        let sort = |code: usize| {
            Some(Sort {
                column: Column::from_index(code / 2)?,
                descending: !code.is_multiple_of(2),
            })
        };
        let sort = [sort(parse(parts.next()?)?)?, sort(parse(parts.next()?)?)?];
        let paths = [
            PathBuf::from(OsString::from_wide(parts.next()?)),
            PathBuf::from(OsString::from_wide(parts.next()?)),
        ];
        if paths.iter().any(|path| !path.is_absolute()) {
            return None;
        }
        Some(Self {
            paths,
            sort,
            split: position.clamp(0.30, 0.70),
            theme,
            persist: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_preserves_unicode_and_rejects_invalid_data() {
        let workspace = Workspace {
            paths: [
                PathBuf::from(r"C:\År"),
                PathBuf::from(r"\\server\share\日本語"),
            ],
            sort: [
                Sort::default(),
                Sort {
                    column: Column::Modified,
                    descending: true,
                },
            ],
            split: 0.615,
            theme: Theme::Dark,
            persist: true,
        };
        let data = workspace.encode();
        let saved = Workspace::decode(&data).unwrap();
        assert_eq!(saved.paths, workspace.paths);
        assert!(saved.sort == workspace.sort);
        assert_eq!(saved.split, workspace.split);
        assert!(matches!(saved.theme, Theme::Dark));
        assert!(Workspace::decode(&data[..5]).is_none());
    }
}
