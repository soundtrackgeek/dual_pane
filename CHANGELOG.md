# Changelog

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
