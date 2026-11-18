# Dual Pane

A keyboard-first file manager for Windows 11. Browse two folders side by side and copy or move selected files and folders to the opposite pane.

## Build and run

Build with stable Rust for Windows, using the MSVC target and Visual Studio Build Tools with the Desktop development with C++ workload and a Windows SDK.

```powershell
cargo build --release --locked
.\target\release\dual-pane.exe
```

The app starts with your user profile on the left and the working directory on the right. Pass up to two paths to open specific folders. Quote paths that contain spaces.

```powershell
.\target\release\dual-pane.exe "$env:USERPROFILE\Documents" "D:\Archive"
```

## Browse and transfer files

| Shortcut | Action |
| --- | --- |
| Tab / Shift+Tab | Switch to the opposite pane |
| Ctrl+L | Select the active pane's folder path for editing |
| Enter | Go to the edited path, open the focused folder, or open the focused file in its Windows-associated app |
| Esc | Restore the current path while editing |
| Backspace | Go to the parent folder from the file list |
| Ctrl+R | Refresh the active pane |
| Ctrl+A | Select all rows in the active file list |
| F5 / F6 | Copy / move the selection to the opposite pane after confirmation |
| Alt+V | Open the View menu |

You can double-click a row to open it, use Ctrl or Shift to select multiple rows, and drag the divider to resize the panes. The active pane is marked above its path field. The View menu offers System, Light, and Dark themes.

The lists use folders-first natural name order. Both local and UNC paths work for browsing. Use Ctrl+R to pick up changes made by other apps. Dual Pane refreshes affected folders automatically after its own transfers.

## Implementation

Dual Pane uses Rust, Microsoft's `windows` crate, and Win32 common controls. Each pane uses a virtual list view and background folder enumeration with a bounded work queue. Large listings show an early batch while the complete folder is read and sorted. Rows use shared Windows icons and format their metadata on demand. Copy and move use Windows Shell file operations with native conflict handling.

```powershell
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

## Milestone measurements

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
