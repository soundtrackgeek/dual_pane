# Changelog

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
