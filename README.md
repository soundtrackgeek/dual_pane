# Dual Pane

A keyboard-first file manager for Windows 11. Browse, filter, and organize two folders side by side.

## Build and run

Build with stable Rust for Windows, using the MSVC target and Visual Studio Build Tools with the Desktop development with C++ workload and a Windows SDK.

```powershell
cargo build --release --locked
.\target\release\dual-pane.exe
```

The first launch opens your user profile on the left and the working directory on the right. On later launches the app restores both paths, per-pane sorting, the divider, and the theme from your Windows user settings. Pass up to two paths to override the saved paths for launch. Quote paths that contain spaces.

```powershell
.\target\release\dual-pane.exe "$env:USERPROFILE\Documents" "D:\Archive"
```

Add `--no-workspace` for a temporary session that starts with the defaults and leaves the saved workspace alone.

## Browse and organize files

| Shortcut | Action |
| --- | --- |
| Tab / Shift+Tab | Switch to the opposite pane |
| Ctrl+L | Select the active pane's folder path for editing |
| Enter | Go to the edited path, return to the list from the filter, or open the focused item |
| Esc | Restore the current path while editing, or clear the focused filter and return to the list |
| Backspace | Go to the parent folder from the file list |
| Ctrl+R | Refresh the active pane |
| Ctrl+A | Select all visible rows in the active file list |
| Ctrl+F | Focus the active pane's filename filter |
| Ctrl+1 / Ctrl+2 / Ctrl+3 | Sort by Name / Size / Modified. Repeat to reverse direction |
| F2 | Rename the selected file or folder |
| F5 / F6 | Copy / move the selection to the opposite pane after confirmation |
| F7 | Create a folder in the active pane |
| Delete | Confirm sending the selection to the Recycle Bin |
| Alt+V | Open the View menu |

You can double-click a row to open it, use Ctrl or Shift to select multiple rows, and drag the divider to resize the panes. The active pane is marked above its path field. The View menu offers System, Light, and Dark themes.

The lists start in folders-first natural name order. Click **Name**, **Size**, or **Modified** to sort and click the same column again to reverse the order. The filter matches any part of a filename, ignores case, and clears when you navigate to another folder. The pane footer shows the filtered count and the selected count. Rows hidden by a filter leave the selection.

Both local and UNC paths work for browsing. Dual Pane watches each open folder for changes and preserves the selected items and viewport when possible. An unsupported or disconnected watch leaves **Ctrl+R** available for a manual refresh. If Windows cannot recycle the selection, it presents a separate warning before a permanent deletion.

## Implementation

Dual Pane uses Rust, Microsoft's `windows` crate, and Win32 common controls. Each pane uses a virtual list view and background folder enumeration with a bounded work queue. Large listings show an early batch while the complete folder is read and sorted. Rows use shared Windows icons and format their metadata on demand. Filename filtering and sorting run on the same background workers. Folder watches use asynchronous Windows change notifications and group bursts of changes before rescanning. Copy, move, rename, and recycle use Windows Shell file operations.

```powershell
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

## 0.2.0 verification

The x64 release build was retested on the same Windows 11 PC with a Ryzen 5 7600X and 32 GiB of RAM. One pane held 100,000 local entries and the other was empty. Timings are medians of five fresh processes with a warm filesystem cache.

| Measurement | Result |
| --- | ---: |
| Launch to visible window | 69 ms |
| Launch to first rows | 93 ms |
| Launch to all rows | 268 ms |
| Tab switching during the scan | Under 4 ms in five trials |
| Private working set with the large list loaded | 10.8 MiB |
| Process CPU time over 10 seconds at idle | 0 ms recorded |
| Release executable | 449 KiB |

The Windows smoke test covered filtered rename and move, live updates with selection and scroll preservation, workspace restart, and a read-only 5,478-entry remote SMB folder. It also confirmed that declining a permanent-delete warning on a loopback SMB share keeps the file. The automated test covers a simulated 150% DPI change. A slower PC, physical mixed-DPI monitors, and a high-latency share are still untested.

## 0.1.0 baseline measurements

The x64 release build was measured on Windows 11 with a Ryzen 5 7600X and 32 GiB of RAM. Startup numbers are medians of five fresh processes with a warm filesystem cache, one pane pointed at a 100,000-entry local folder, and the other pane pointed at a small folder.

| Measurement | Result |
| --- | ---: |
| Launch to visible window | 63 ms |
| Launch to first rows | 82 ms |
| Launch to all rows | 250 ms |
| Tab switching during the scan | Under 4 ms in five trials |
| Private working set with the large list loaded | 10.7 MiB |
| Total working set with the large list loaded | 31.2 MiB |
| Process CPU time over 10 seconds at idle | 0 ms recorded |
| Release executable | 319 KiB |

The Windows smoke test exercised keyboard navigation, copy, move with a Unicode filename, the light and dark themes, and a 100,000-entry loopback SMB folder. It also exercised pane switching and local recovery during an unavailable UNC request. The automated DPI test sends a simulated 150% scale change to the real Win32 controls. Physical mixed-DPI monitors and a high-latency SMB share are still untested.
