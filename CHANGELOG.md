# Changelog

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
