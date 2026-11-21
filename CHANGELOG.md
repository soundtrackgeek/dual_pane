# Changelog

## [1.0.0-rc.10] - 2026-11-21

### Added

- Added **Pack to ZIP…** (`Alt+F5`, View menu, and item context menu) to compress selected disk files and folders into a `.zip` archive through the Transfers queue with streamed Deflate writes, ZIP64 support, progress, cancellation, and atomic no-overwrite publishing.
- Added **Quick View (`F3`) inside ZIP archives**, streaming bounded text (up to 128 KiB) and image (up to 24 MiB) previews directly from archive members without extracting to disk.

## [1.0.0-rc.9] - 2026-11-21

### Added

- Added extension-aware Windows file-type icons cached per lowercase extension with zero per-row disk I/O.
- Added a **Show hidden items** toggle (`Ctrl+H` and **View → Show hidden items**), persisted in workspace settings, with muted text styling for hidden/system rows and a hidden-item count in the pane footer when hidden.

## [1.0.0-rc.8] - 2026-11-21

### Changed

- Right-clicking a file or folder now toggles its selection, and holding and dragging the right mouse button across rows selects or deselects them continuously.
- Holding the right mouse button in place for 2 seconds opens the item or folder context menu (and preserves selection if held on an already-selected item); `Shift+F10` and the Menu key still open it immediately.

## [1.0.0-rc.7] - 2026-11-21

### Added

- Added a right-click and keyboard (`Shift+F10` / Menu key) context menu for file lists, with item, folder-background, and ZIP archive actions.
- Added everyday Windows shell actions from both the context menu and View menu: **Copy path(s)** (`Shift+Alt+C`), **Open in Terminal** (`Alt+T`, preferring Windows Terminal with PowerShell/cmd fallback), **Open in File Explorer** (selecting the focused item), and **Properties** (`Alt+Enter`).

## [1.0.0-rc.6] - 2026-11-18

### Fixed

- The File menu bar and dropdown now follow the effective theme, including Windows dark mode when Filehelm is set to System.

## [1.0.0-rc.5] - 2026-11-18

### Added

- File → Update now downloads and verifies the matching Windows package, installs it, and reopens Filehelm. It supports both MSI installations and portable copies.
- Update installation waits for active and queued file transfers to finish, and saves the workspace before closing Filehelm.

## [1.0.0-rc.4] - 2026-11-18

### Fixed

- Fixed stale drive-button lettering after resizing or moving the pane divider, including at 125% display scaling.

## [1.0.0-rc.3] - 2026-11-18

### Added

- Added a File menu with appearance settings, an on-demand GitHub release check, and an About dialog with the running version.
- Added download links for newer releases with a Windows installer and portable ZIP. Stable builds check for stable updates, and prerelease builds also check for newer prereleases.

## [1.0.0-rc.2] - 2026-11-18

### Added

- Added per-pane drive buttons for assigned drive letters, with the current drive highlighted and an overflow menu for narrow panes.
- Added Ctrl+D to open the active pane’s drive menu. Drive letters refresh when the app is activated, when Windows reports a device change, and on manual refresh.

## [1.0.0-rc.1] - 2026-11-18

### Added

- Added the Filehelm Windows x64 per-user MSI, Start menu shortcut, and portable ZIP.
- Added version-triggered Windows package verification and GitHub Releases publication.
- Pinned the Rust toolchain used for local and GitHub release builds.

### Changed

- Renamed the application to Filehelm and added a native icon and Windows version information.

### Fixed

- Improved file-list column sizing in narrow panes and validated the main layout at 100%, 150%, and 200% scaling.

## [0.11.0] - 2026-11-18

### Added

- Added Ctrl+Z and Operation History for confirmed file renames, batch renames, and same-volume moves, including completed items from canceled operations.
- Added per-file undo review with file identity, metadata, folder, and destination checks and a no-overwrite restore.
- Added bounded local history that survives restarts and records restored items.

## [0.10.0] - 2026-11-18

### Added

- Added Windows file cut, copy, and paste for disk panes, with support for multiple files and folders and cut highlighting.
- Added native drag-and-drop into and between file lists with destination and copy or move feedback.
- Routed clipboard and drop operations through Transfers, including items from multiple source folders.

## [0.9.0] - 2026-11-18

### Added

- Added read-only ZIP and ZIP64 browsing in either pane, including nested folders, filtering, sort, history, and tab restore.
- Added F5 extraction through the Transfers queue, with streamed writes, progress, cancellation, per-item outcomes, and protection for existing destination names.
- Added archive source rechecks, safe member path validation, and staging to keep incomplete or corrupt items out of the final destination.

## [0.8.0] - 2026-11-18

### Added

- Added Batch Rename for selected files, with literal replacement, prefix and suffix rules, sequence numbering, and optional extension preservation.
- Added a virtual side-by-side preview that flags invalid names, duplicate targets, occupied destinations, and unsupported selections.
- Added file identity and destination rechecks, no-overwrite renames, and per-file results when a batch finishes or stops.

## [0.7.0] - 2026-11-18

### Added

- Added an F3 Quick View window for read-only text, JPEG, PNG, and GIF previews, with file details and an Open action.
- Added keyboard navigation through visible files while preserving the source pane's selection and scroll position.
- Added bounded background reads and image decoding, with superseded previews discarded when switching files or closing the viewer.

## [0.6.0] - 2026-11-18

### Added

- Added a modeless Find Files window for recursive, case-insensitive filename and wildcard searches under one or both active folders.
- Added streaming results, Stop, and a skipped-folder summary. Searches run on demand and skip directory junctions and other linked folders.
- Added a shortcut from each result to a new tab on its source side, with the matching file selected.

## [0.5.0] - 2026-11-18

### Added

- Added a serial queue for F5 copies, F6 moves, and reviewed copies that retains the confirmed paths and checks reviewed jobs when they start.
- Added a modeless Transfers panel with Windows progress estimates, per-item outcomes, and recent session history.
- Added queue pause and resume, removal of waiting transfers, active cancellation requests, and retry with original-file and destination checks.

## [0.4.0] - 2026-11-18

### Added

- Added a comparison of immediate folder contents with case-insensitive name matching and filters for missing, newer, and metadata-matched entries.
- Added cancellable byte comparison for selected regular-file pairs and a command to open matching folders in both panes.
- Added directional copy review with exact destination names, replacement labels, stale-plan checks, and Windows copy confirmation.

## [0.3.0] - 2026-11-18

### Added

- Added independent folder tabs in each pane with keyboard shortcuts and per-tab Back and Forward history.
- Added named folder pairs with a picker that previews both paths before opening them.
- Added startup restore for both tab strips and their active tabs, filters, and view positions. Existing 0.2.0 workspaces open with one tab per pane.

## [0.2.0] - 2026-11-18

### Added

- Added live folder updates with selection, scroll position, and in-progress path edits preserved.
- Added F2 rename, F7 new folder, and Delete to recycle with a separate Windows warning when permanent deletion is required.
- Added per-pane filename filtering and sortable Name, Size, and Modified columns, also available from the keyboard.
- Added automatic workspace restore for both folder paths, sort, divider position, and theme, with a `--no-workspace` option for temporary sessions.

## [0.1.0] - 2026-11-18

### Added

- Added an initial native Windows 11 file manager with two resizable virtual file lists, editable paths, and keyboard navigation.
- Added background browsing for local and UNC folders, early rows during large scans, and protection against late results from superseded paths.
- Added F5 copy and F6 move between panes with destination confirmation and Windows Shell conflict handling.
- Added system, light, and dark themes, per-monitor DPI handling, and Microsoft Active Accessibility names for both folder paths and lists.
