use std::path::Component;

use super::*;

impl App {
    pub fn navigate_zip(&self, index: usize, path: PathBuf, folder: Arc<str>) {
        self.go_archive_location(index, Location::archive(path, folder));
    }

    fn go_archive_location(&self, index: usize, location: Location) {
        self.capture_tab(index);
        self.panes[index]
            .navigation
            .borrow_mut()
            .current_mut()
            .go(location);
        self.activate_tab(index);
    }

    pub fn finish_archive(
        &self,
        index: usize,
        request: usize,
        archive: std::io::Result<Arc<crate::archive::Index>>,
    ) {
        let pane = &self.panes[index];
        let mut listing = pane.listing.borrow_mut();
        if listing.request != request || listing.archive.is_none() {
            return;
        }
        listing.loading = false;
        listing.complete = true;
        listing.error = match archive {
            Ok(archive) => {
                let entries = archive.entries(listing.archive.as_deref().unwrap());
                listing.archive_index = Some(archive);
                if let Some(entries) = entries {
                    listing.entries = entries;
                    None
                } else {
                    Some("This folder is no longer in the ZIP.".into())
                }
            }
            Err(error) => Some(error.to_string()),
        };
        if listing.error.is_some() {
            listing.entries = Arc::new(Vec::new());
        }
        drop(listing);
        self.request_view(index, true);
        self.status_changed();
    }

    pub fn show_pane_path(&self, index: usize) {
        let pane = &self.panes[index];
        let listing = pane.listing.borrow();
        let mut path = listing.path.clone();
        if let Some(folder) = &listing.archive {
            path.extend(folder.split('/').filter(|part| !part.is_empty()));
        }
        drop(listing);
        show_path(pane.path, &path);
    }

    pub fn navigate_path_field(&self, index: usize, edit: &Path) {
        let listing = self.panes[index].listing.borrow();
        let base = listing.path.clone();
        let archive = listing.archive.clone();
        drop(listing);
        let Some(folder) = archive else {
            self.navigate(index, base.join(edit));
            return;
        };
        let inner = if edit.is_absolute() {
            match edit.strip_prefix(&base) {
                Ok(path) => path,
                Err(_) => {
                    self.navigate(index, edit.to_owned());
                    return;
                }
            }
        } else {
            edit
        };
        let mut parts: Vec<&str> = if edit.is_absolute() {
            Vec::new()
        } else {
            folder.split('/').filter(|part| !part.is_empty()).collect()
        };
        for component in inner.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    parts.pop();
                }
                Component::Normal(part) if part.to_str().is_some() => {
                    parts.push(part.to_str().unwrap())
                }
                _ => {
                    self.show_pane_path(index);
                    return;
                }
            }
        }
        let target = parts.join("/");
        if !crate::archive::valid_folder(&target) {
            self.show_pane_path(index);
        } else if target == folder.as_ref() {
            self.refresh(index);
        } else {
            self.navigate_zip(index, base, target.into());
        }
    }

    pub fn archive_parent(&self, index: usize) -> bool {
        let listing = self.panes[index].listing.borrow();
        let Some(folder) = &listing.archive else {
            return false;
        };
        let path = listing.path.clone();
        let mut parent = if folder.is_empty() {
            let Some(parent) = parent_of(&path) else {
                return true;
            };
            Location::new(parent)
        } else {
            let parent = folder.rsplit_once('/').map_or("", |(parent, _)| parent);
            Location::archive(path.clone(), Arc::from(parent))
        };
        let name = if folder.is_empty() {
            path.file_name()
                .map(|name| name.encode_wide().collect::<Vec<_>>())
                .unwrap_or_default()
        } else {
            folder.rsplit('/').next().unwrap().encode_utf16().collect()
        };
        parent.anchors.focused = name.clone();
        parent.anchors.selected = name;
        drop(listing);
        self.go_archive_location(index, parent);
        true
    }

    pub fn open_archive_row(&self, index: usize, row: isize) -> bool {
        let listing = self.panes[index].listing.borrow();
        let archive = listing.archive.clone();
        let Some(entry) = listing.rows.get(row as usize) else {
            return archive.is_some();
        };
        let name = entry.filename();
        let path = listing.path.clone();
        let directory = entry.directory;
        drop(listing);
        if let Some(folder) = archive {
            if directory {
                let name = name.to_str().unwrap();
                let next = if folder.is_empty() {
                    name.to_owned()
                } else {
                    format!("{folder}/{name}")
                };
                self.navigate_zip(index, path, next.into());
            } else {
                *self.operation_status.borrow_mut() =
                    "Press F5 to extract selected ZIP members.".into();
                self.status_changed();
            }
            return true;
        }
        let path = path.join(name);
        if !directory
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
        {
            self.navigate_zip(index, path, Arc::from(""));
            return true;
        }
        false
    }

    pub fn extract_selection(&self, index: usize, destination: PathBuf) {
        let pane = &self.panes[index];
        let listing = pane.listing.borrow();
        if !listing.ready() {
            return;
        }
        let (Some(archive), Some(folder)) = (&listing.archive_index, &listing.archive) else {
            return;
        };
        let files = pane
            .selection()
            .filter_map(|row| listing.rows.get(row))
            .map(crate::transfer::Requested::selected)
            .collect::<Vec<_>>();
        if files.is_empty() {
            return;
        }
        let path = listing.path.clone();
        let source = archive.source(folder.clone());
        let omitted = archive.omitted;
        drop(listing);

        let mut text = format!(
            "Extract {} to\n{}?",
            selection_label(files.len()),
            destination.display()
        );
        for file in files.iter().take(5) {
            text.push_str(&format!("\n  {}", file.name.to_string_lossy()));
        }
        if files.len() > 5 {
            text.push_str(&format!(
                "\n  … and {} more",
                grouped((files.len() - 5) as u64)
            ));
        }
        text.push_str("\n\nExisting destination names will be skipped.");
        if omitted > 0 {
            text.push_str(&format!(
                "\n\n{} unsafe or unsupported entries were omitted from this ZIP.",
                grouped(omitted as u64)
            ));
        }
        if message(
            self.window,
            "Extract ZIP members",
            &text,
            MB_OKCANCEL | MB_ICONQUESTION | MB_DEFBUTTON2,
        ) == IDOK
        {
            self.enqueue_transfer(crate::transfer::Request::extract(
                [path, destination],
                files,
                source,
            ));
            self.show_transfers();
        }
        self.focus_list(index);
    }

    pub fn pack_selection(&self) {
        let index = self.active.get();
        let pane = &self.panes[index];
        if !self.can_queue_transfer() {
            return;
        }
        let (folder, files) = {
            let listing = pane.listing.borrow();
            if !listing.disk_ready() {
                return;
            }
            let files = pane
                .selection()
                .filter_map(|row| listing.rows.get(row))
                .map(crate::transfer::Requested::selected)
                .collect::<Vec<_>>();
            (listing.path.clone(), files)
        };
        if files.is_empty() {
            return;
        }
        let destination = {
            let target = self.panes[1 - index].listing.borrow();
            if target.disk_ready() {
                target.path.clone()
            } else {
                folder.clone()
            }
        };
        let default_name = default_zip_name(&folder, &files);
        let stem_len = default_name
            .to_string_lossy()
            .strip_suffix(".zip")
            .map_or_else(
                || default_name.encode_wide().count(),
                |stem| stem.encode_utf16().count(),
            );
        let name = prompt::name(
            self.window,
            self.font.get(),
            w!("Pack to ZIP"),
            &default_name,
            stem_len,
        );
        self.focus_list(index);
        if let Some(name) = name {
            let archive_name = ensure_zip_extension(name);
            self.enqueue_transfer(crate::transfer::Request::pack(
                [folder, destination],
                files,
                archive_name,
            ));
            self.show_transfers();
        }
    }
}

fn default_zip_name(folder: &Path, files: &[crate::transfer::Requested]) -> OsString {
    if files.len() == 1 {
        let stem = Path::new(&files[0].name)
            .file_stem()
            .filter(|stem| !stem.is_empty())
            .unwrap_or(&files[0].name);
        let mut name = stem.to_os_string();
        name.push(".zip");
        name
    } else if let Some(folder_name) = folder.file_name().filter(|name| !name.is_empty()) {
        let mut name = folder_name.to_os_string();
        name.push(".zip");
        name
    } else {
        OsString::from("archive.zip")
    }
}

fn ensure_zip_extension(mut name: OsString) -> OsString {
    if !Path::new(&name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
    {
        name.push(".zip");
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_default_zip_names_and_appends_extension() {
        assert_eq!(
            ensure_zip_extension(OsString::from("backup")),
            OsString::from("backup.zip")
        );
        assert_eq!(
            ensure_zip_extension(OsString::from("backup.ZIP")),
            OsString::from("backup.ZIP")
        );
        assert_eq!(
            default_zip_name(Path::new(r"C:\"), &[]),
            OsString::from("archive.zip")
        );
    }
}
