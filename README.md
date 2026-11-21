# Filehelm

A keyboard-first file manager for Windows 11. Browse, search, compare, and organize folders side by side.

## Install on Windows 11

Download the Windows x64 MSI or portable ZIP from [GitHub Releases](https://github.com/soundtrackgeek/dual_pane/releases). Run the MSI to install Filehelm for your Windows account and add a Start menu shortcut. To run without installing, extract the portable ZIP and open `filehelm.exe`.

Both packages use the same per-user workspace and Undo history, including existing data from builds under the Dual Pane name. Installing a newer MSI upgrades the app in place. Uninstalling through Windows Settings keeps your workspace and Undo history. The release candidate is unsigned, so Windows may prompt before opening it.

## Settings, updates, and version

Open the **File** menu with **Alt+F**. Choose **Settings** to switch between the System, Light, and Dark themes. The File menu and its bar follow the selected theme. In System mode, they follow the Windows app theme. Filehelm applies the theme when you confirm and saves it with your workspace on exit. Choose **About** to see the version you’re running.

Choose **File → Update**, then **Install update**. Filehelm downloads and verifies the update, saves your workspace, and reopens when installation finishes. It runs the MSI for installed copies and replaces only the executable for portable copies. If file operations are active or transfers are queued, finish them and choose **File → Update** again to apply the downloaded update. Stable builds offer stable updates, and prerelease builds also offer newer prereleases.

If you’re running rc.4 or earlier, install this release from GitHub once to get the in-app installer.

## Build and run

Build with the Rust version pinned in `rust-toolchain.toml`, using the MSVC target and Visual Studio Build Tools with the Desktop development with C++ workload and a Windows SDK.

```powershell
cargo build --release --locked
.\target\release\filehelm.exe
```

The first launch opens your user profile on the left and the working directory on the right. Later launches restore the open tabs and selected tab in each pane, saved folder pairs, the divider, and the theme from your Windows user settings. Pass up to two paths to override the active tabs for launch. Quote paths that contain spaces.

```powershell
.\target\release\filehelm.exe "$env:USERPROFILE\Documents" "D:\Archive"
```

Add `--no-workspace` for a temporary session that starts with the defaults and leaves the saved workspace alone.

To build the installer, portable archive, and SHA-256 checksums locally, run `./scripts/package-windows.ps1` in PowerShell 7 with the .NET 10 SDK installed. Output goes to `target/dist`. The GitHub Actions workflow builds, tests, and publishes packages for each Cargo version bump pushed to `master`. Versions with a prerelease suffix appear under GitHub prereleases.

## Browse and organize files

Click a drive button above a pane’s path field to open that drive’s root in its active tab. The current drive is highlighted. Choose **Drives…** when the buttons overflow, or press **Ctrl+D** to open the active pane’s full drive menu. The list includes local, removable, and mapped drives with Windows drive letters. Filehelm refreshes it when the window regains focus, when Windows reports a device change, and when you press **Ctrl+R**.

| Shortcut | Action |
| --- | --- |
| Tab / Shift+Tab | Switch to the opposite pane |
| Ctrl+T / Ctrl+W | Duplicate the active tab / close the active tab |
| Ctrl+Tab / Ctrl+Shift+Tab | Cycle tabs forward / backward in the active pane |
| Alt+Left / Alt+Right | Go back / forward in the active tab |
| Ctrl+L | Select the active pane's folder path for editing |
| Enter | Go to the edited path, return to the list from the filter, or open the focused item |
| Esc | Restore the current path while editing, or clear the focused filter and return to the list |
| Backspace | Go to the parent folder from the file list |
| Ctrl+R | Refresh the active pane |
| Ctrl+D | Open the active pane’s drive menu |
| Ctrl+A | Select all visible rows in the active file list |
| Ctrl+X / Ctrl+C / Ctrl+V | Cut / copy / paste Windows files and folders in a file list |
| Ctrl+Z | Review the latest operation with files available for undo |
| Ctrl+F | Focus the active pane's filename filter |
| Alt+F7 | Find files in subfolders |
| Ctrl+1 / Ctrl+2 / Ctrl+3 | Sort by Name / Size / Modified. Repeat to reverse direction |
| Ctrl+H | Show or hide hidden and system files |
| Ctrl+Shift+C | Compare the folders in the two active tabs |
| Ctrl+J | Open the Transfers panel |
| Ctrl+P | Open the folder-pair picker |
| Ctrl+Shift+P | Save the two visible folders as a named pair |
| F2 | Rename the selected file or folder |
| Ctrl+M | Review a batch rename for the selected files |
| F3 | Preview the focused file in Quick View |
| F5 / F6 | Queue a confirmed copy / move to the opposite pane. F5 extracts from a ZIP. |
| F7 | Create a folder in the active pane |
| Delete | Confirm sending the selection to the Recycle Bin |
| Shift+Alt+C | Copy the full path(s) of the selected items or active folder to the clipboard |
| Alt+T | Open a terminal in the active folder (or the single selected folder) |
| Alt+Enter | Open Windows Properties for the selected item or active folder |
| Shift+F10 / Hold right-click (2s) | Open the item or folder context menu in a file list |
| Alt+V | Open the View menu |

You can double-click a row to open it, right-click to toggle selection or hold and drag with the right mouse button to select multiple rows, hold right-click for 2 seconds (or press **Shift+F10** / the Menu key) for the context menu (including **Open in File Explorer**, **Open in Terminal**, **Copy path**, and **Properties**), use Ctrl or Shift to select multiple rows, and drag the divider to resize the panes. The active pane is marked above its path field. The View menu offers System, Light, and Dark themes.

Each tab remembers its folder, sort, filter, scroll position, and focused item. A single selected item is restored when it is still visible. Reopening a tab reloads its folder. Each pane keeps at least one tab, supports up to 32 tabs, and keeps Back/Forward history for the current session.

Use **Pairs…** to preview the left and right paths for a saved pair. Opening a pair navigates the two active tabs and leaves the other tabs available. The picker also lets you save or remove a pair.

The lists start in folders-first natural name order and show Windows file-type icons cached by extension. Hidden and system items are hidden by default and counted in the pane footer; press **Ctrl+H** or choose **View → Show hidden items** to show them in muted text (persisted with your workspace). Click **Name**, **Size**, or **Modified** to sort and click the same column again to reverse the order. The filter matches any part of a filename and ignores case. New folder visits start with a clear filter. Back and Forward return to the filter for that visit. The pane footer shows the filtered count and the selected count. Rows hidden by a filter leave the selection.

Both local and UNC paths work for browsing. Filehelm watches each open folder for changes and preserves the selected items and viewport when possible. An unsupported or disconnected watch leaves **Ctrl+R** available for a manual refresh. If Windows cannot recycle the selection, it presents a separate warning before a permanent deletion.

## Undo and Operation History

Press **Ctrl+Z** from a file list, or choose **View → Operation History…**. The review shows the recorded path, the original path, and the state of each file. Select an operation to look back through the history. **Undo ready** confirms and restores the eligible files.

History records confirmed regular-file renames, batch renames, and same-volume moves into empty destinations. A canceled operation contributes the files that completed. Undo checks the file identifier, metadata, and folder identities, and uses a no-overwrite rename. If a file changed or the original name is occupied, that row is blocked. Use **Refresh** to review again after clearing a conflict.

The most recent 24 operations and up to 20,000 file changes are kept within a 16 MiB history budget under `%LOCALAPPDATA%\DualPane\History`. A `--no-workspace` session keeps its undo history in memory until it closes.

## Windows clipboard and drag-and-drop

Select files or folders in a disk pane and press **Ctrl+C** or **Ctrl+X**. Paste with **Ctrl+V** in another pane, tab, or File Explorer. You can also copy or cut in File Explorer and paste into an open disk pane. Cut items are dimmed while Filehelm owns the cut clipboard. The path and filter fields use ordinary text clipboard shortcuts.

Drag files or folders from File Explorer into a file list or between Filehelm lists. The highlighted pane shows the destination and the proposed action. A drop copies by default when copying is available. Hold **Shift** to move or **Ctrl** to copy. The destination is the highlighted pane's open folder.

Paste and drop capture the destination and queue the work in **Transfers**. Windows handles conflicts. Completed items remain in place when you cancel, and the panel reports each item. A fully completed clipboard move clears that cut only if the clipboard still holds the same data. Clipboard and drop support applies to Windows filesystem paths. Use **F5** to extract ZIP members.

## ZIP archives

Focus a `.zip` file and press **Enter** or double-click it to browse the archive in that pane. Open its subfolders, sort the list, or use **Ctrl+F** to filter the current ZIP folder. **Backspace** goes up one level and leaves the ZIP at its root. **Alt+Left** and **Alt+Right** work with ZIP locations, and saved tabs can reopen a ZIP folder. Use **Ctrl+R** to reread an archive that changed.

Select members and press **F5** to confirm extraction to the disk folder in the opposite pane. A selected folder includes its descendants. Follow the job in **Transfers**. Existing names are skipped, including destination folders. A member is published only after it has been written and checked. If you cancel, completed items remain in place and the current partial item is discarded.

Browsing and extraction support read-only ZIP and ZIP64. Extraction supports stored and Deflate members. Password-protected and unsupported compression methods show an error in the job results. Entries with invalid Windows names, paths outside the archive folder, or links are omitted and counted in the pane. The archive is rechecked when a queued job starts.

## Batch Rename

Select regular files in one pane and press **Ctrl+M**, or choose **View → Batch Rename…**. The tool captures that selection in its visible order. Enter a literal, case-sensitive **Find** and **Replace** rule, add a prefix or suffix, or append a space and a sequence number. Set **Start at** and **Digits** to control numbering. **Keep extensions** is on by default.

Click **Review** to see each original name, proposed name, and status. Changing a rule clears the review. Invalid names, duplicate targets, existing names, folders, and linked files must be resolved before **Apply** is enabled. A target occupied by another selected file is treated as a conflict. Case-only changes to the original name are supported.

**Apply** rechecks the reviewed file identities and destinations, then renames without overwriting. If a check fails before any rename, review again after resolving the problem. If Windows stops the batch partway through, the result shows which names changed and which were unattempted. Close and reopen Batch Rename to start from a fresh selection.

## Quick View

Focus a file in either pane and press **F3**, or choose **View → Quick View…**. The read-only viewer shows the filename, size, and modified date. Use **Left** and **Right** or **Previous** and **Next** to step through visible files in the source pane. When the text area has focus, use **Alt+Left** or **Alt+Right** to switch files. Press **Esc** to close the viewer. Previewing leaves the pane's selection and scroll position in place.

Quick View shows up to the first **128 KiB** of UTF-8 or BOM-marked UTF-16 text. It fits JPEG, PNG, and the first frame of GIF images to the window and honors JPEG orientation. It reads up to **24 MiB** of compressed image data and caps the preview bitmap at **1,600 pixels** per side. Images over **16 megapixels** require the decoder to supply scaled output. Use **Open** to launch the file's associated app, including for formats outside Quick View's text and image support.

Preview reads run on demand in the background. If a network read stalls, you can close the viewer or switch files. Later results for the original file are discarded. At most two preview workers run at once. If both are waiting on Windows, the newest request waits for a worker.

## Find files

Press **Alt+F7** or choose **View → Find files…**. Enter part of a filename or use a wildcard such as `*.flac`. A wildcard matches the whole filename, with `*` matching zero or more characters and `?` matching one. Searches ignore case. Choose the active folder or both folders to search their subfolders. The app uses the folders open when you press **Find**.

Results appear as files are found, and you can keep browsing during the search. **Stop** keeps the matches already displayed. If Windows is waiting for a network response, the worker can take longer to exit. The search skips directory junctions, symlinks, and other directory reparse points and reports skipped and unreadable folders when it finishes. Results are capped at 100,000 rows and a 32 MiB storage budget. Narrow the query when the result limit is reached.

Select a match and press **Enter**, double-click it, or choose **Open folder in new tab**. The app opens a new tab on the source side and selects the file. The Find Files window hides after the jump. Press **Alt+F7** again to return to the results. **Close** ends any active search and clears its results.

## Compare folders

Open **View → Compare folders…** or press **Ctrl+Shift+C**. The comparison uses the immediate contents of both active folders, including entries hidden by a pane filter. Match groups include **Left only**, **Right only**, **Newer on left**, **Newer on right**, and **Same size and time**. Select one matching folder and choose **Open both folders** to compare its contents at the next level.

**Compare contents** checks the bytes of selected regular-file pairs. **Same size and time** describes the metadata. **Contents equal** means the byte check passed. The byte check streams the files in bounded chunks. **Cancel work** stops after an outstanding Windows read returns.

To copy a group, choose it under **Show**, click **Select shown files**, and choose **Review to left** or **Review to right**. The review shows the exact names and distinguishes new files from replacements. **Apply copy** checks both folder identities and rereads the reviewed names and metadata before queuing it. It blocks a stale review and marks the affected rows when it can identify them. Use **Refresh** to compare again and review the updated selection. The queued job rechecks its review when it reaches the front. Windows handles any replacement confirmation during the copy.

Reviewed copy supports regular files. Name collisions, file/folder conflicts, folders, and reparse points are excluded from batch selection. File metadata and available folder notifications are used for freshness checks.

## Transfers

Open **Transfers** or press **Ctrl+J**. Copies, moves, reviewed copies, ZIP extractions, and clipboard and drop jobs run in order, one job at a time. The job keeps the source paths and destination captured when it was queued. Each source is checked when the job starts.

The panel stays open while you browse. Select a job to see its paths, Windows' progress estimate, and results for the selected top-level items. Windows shows its file conflict and progress dialogs when needed. **Pause queue** holds the next job, **Remove waiting** cancels a job before it starts, and **Cancel transfer** asks Windows to stop the active job at its next cancellation point. Completed items remain in place.

**Retry verified** queues copy and move candidates from the selected job. At the front of the queue it checks the original regular file's identity and metadata and requires its destination name to be empty. Use **Open folders** to return both panes to the job's source and destination. For ZIP extraction, select the remaining members and press **F5** again.

The queue and its transfer reports belong to the current app session. Confirmed, eligible moves also appear in Operation History. Closing the app with waiting jobs asks whether to discard them.

## Implementation

Filehelm uses Rust, Microsoft's `windows` crate, and Win32 common controls. Each pane uses one virtual list view and one folder watcher. Inactive tabs retain navigation state. Background folder enumeration uses a bounded work queue. Large listings show an early batch while the complete folder is read and sorted. Rows use shared Windows icons and format their metadata on demand. Filename filtering and sorting run on the same background workers. Folder watches use asynchronous Windows change notifications and group bursts of changes before rescanning. Copy, move, single-item rename, and recycle use Windows Shell file operations. Batch Rename uses native file handles and a virtual preview list.

```powershell
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

## Filehelm 1.0 release candidate verification

On Windows 11 x64, five warm-cache runs opened a 100,000-entry local folder in a median of **87 ms to first rows** and **123 ms to the full list**. A 100,000-entry ZIP64 populated in a median of **123 ms** after pressing Enter. Median settled private working set was **10.9 MiB** for either listing. The ZIP's median sampled peak was **30.3 MiB**. Each listing recorded **0 ms** of process CPU time in a separate 10-second idle sample. Switching to the local pane took **2.7 ms** while the opposite pane waited for an unavailable loopback UNC path.

Windows desktop checks covered a clean per-user install, an in-place upgrade from a test MSI built with the 0.11.0 executable, and uninstall. Existing workspace and Undo data were preserved. The portable ZIP and MSI contained the same executable. Desktop checks also covered light and dark list colors, keyboard navigation, copy, move, Undo, and ZIP extraction. The Win32 layout test sends simulated DPI changes at 100%, 150%, and 200%, including narrow panes.

## 0.11.0 verification

Windows desktop checks restored a batch rename after restarting, blocked an original name created after review, and restored the remaining file after the conflict was cleared. They also covered a case-only rename and an in-place edit that kept the same file size and had its write timestamp reset.

In a 3,500-file move canceled after work started, Operation History offered only the completed items. Undo returned those files to their original folder without changing the unattempted files.

## 0.10.0 verification

Windows desktop checks covered File Explorer copy and cut in both directions, Unicode paths and folder transfers, native cut highlighting, an Explorer drop into Filehelm, and both copy and Shift-move drags between panes.

A mixed-source paste kept its destination after switching tabs and preserved clipboard data copied while the job waited. Canceling a 7,000-file clipboard move left the completed files in the destination and retained the remaining originals and the cut clipboard.

## 0.9.0 verification

Over five warm-cache Windows x64 runs, a **100,000-entry ZIP64** populated the pane in a median of **130 ms**. Median sampled peak private working set was **30.11 MiB** during indexing and **11.03 MiB** after the list loaded. Windows recorded **0 ms** of process CPU time over 10 seconds with the ZIP list idle.

A separate run streamed a **3 GiB** ZIP64 member in **19.6 seconds**, including scripted confirmation, with a sampled peak private working set of **2.74 MiB**. A mid-stream cancellation kept the completed small file and removed the partial large item. Desktop checks also covered Unicode, nested and empty folders, ZIP history and filtering, an existing destination file and junction, and a CRC failure.

## 0.8.0 verification

Over five warm-cache Windows x64 runs, reviewing **10,000 selected files** took a median of **316 ms**. A tab change during review took **7.4 ms**. Median private working set was **4.40 MiB** with the panel open before review and **6.66 MiB** at the sampled peak. Windows recorded **0 ms** of process CPU time over 10 seconds after closing the panel.

A separate Windows run applied 10,000 renames in about **3.2 seconds**, including scripted confirmation. It verified that numbering followed descending sort order and that all renamed files stayed selected. Smaller runs covered Unicode, case-only changes, duplicate and reserved names, a late destination, a same-size replacement with a matching timestamp, and a sharing violation that stopped the batch after one file.

## 0.7.0 verification

Over five warm-cache Windows x64 runs, Quick View showed the excerpt from a **3 GiB** log in a median of **22 ms** and previewed a **48-megapixel** JPEG in **35 ms**. Median private working set was **2.30 MiB** before opening Quick View, **19.22 MiB** at the sampled peak during JPEG decoding, and **2.51 MiB** after closing the viewer. Two 10-second idle samples after closing recorded **0 to 16 ms** of process CPU time.

Native checks covered text, PNG and GIF, JPEG orientation, corrupt and unsupported files, resizing, rapid file changes, and preserving the source selection. A loopback WebDAV test held a preview read open while a local file was previewed. The local preview stayed current when the network response arrived.

## 0.6.0 verification

The Windows x64 release build searched 100,000 files across 100 local subfolders. Over five warm-cache runs, the median was **21 ms to first visible results** and **244 ms to all matches**. The median sampled peak private working set was **10.39 MiB**. After closing Find Files, Windows recorded **0 ms** of process CPU time over 10 seconds at idle.

The desktop checks covered Unicode wildcard matching, junction skipping, unreadable folder reporting, opening a match in a new tab, and F5 and F6 from the revealed selection. With one pane pointed at a stalled loopback WebDAV endpoint, local matches and tab navigation remained responsive. Stop preserved visible results, and the replacement search kept its result after the old connection closed.

## 0.5.0 verification

The Windows x64 build opened a 100,000-entry folder in a median of **91 ms to first rows** and **260 ms to the full list** over five warm-cache launches. Private working set was **10.71 MiB** with Transfers closed and **10.72 MiB** with the empty panel open. Windows recorded **0 ms** of process CPU time over 10 seconds at idle.

Desktop verification covered a 384 MiB copy followed by a queued move, a pause between those jobs, destination retention through tab switches, and native Skip and Cancel outcomes. It also covered a reviewed copy that went stale while waiting, a retry of one verified untouched file, removal of a waiting job, and cancellation from the Transfers panel.

A separate desktop run queued a move while a 3,500-file copy was already running. Canceling the copy after it had started preserved the reported completed items, and the queued move completed.

## 0.4.0 verification

The Windows x64 release build compared two already-loaded 100,000-entry folders in a median of **102 ms** over three runs. The folders produced 110,000 comparison rows. Private working set was **19.43 MiB** with both folders loaded and **20.81 MiB** with the comparison and a filter open. Windows recorded **0 ms** of process CPU time over 10 seconds at idle. Ordinary warm-cache launch reached first rows in **91 ms** and the full 100,000-entry list in **265 ms**, as medians over five runs.

The desktop test covered files with matching size and timestamp but different contents, canceling and retrying a byte check, a destination changed during review, copying a reviewed selection, and opening matched folders in both panes. Canceling Windows' native replacement prompt preserved the destination file and its name.

## 0.3.0 verification

The x64 release build was retested on the same Windows 11 PC. Across five launches with a warm filesystem cache, the median was **93 ms to first rows** and **268 ms to all 100,000 rows**. The private working set was **10.88 MiB with two tabs** and **11.10 MiB with eleven tabs**. Four of those tabs had opened the large folder in turn. Windows recorded **0 ms of process CPU time over 10 seconds at idle**.

The desktop test covered a 0.2.0 workspace upgrade, Back/Forward, saving and reopening a folder pair, and restart restore. It also switched away from an unavailable UNC tab, copied a file to the visible destination, and returned to the original tab and its previous selection, filter, and scroll position.

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
