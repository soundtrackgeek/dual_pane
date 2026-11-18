# Dual Pane

A keyboard-first file manager for Windows 11. Browse, search, compare, and organize folders side by side.

## Build and run

Build with stable Rust for Windows, using the MSVC target and Visual Studio Build Tools with the Desktop development with C++ workload and a Windows SDK.

```powershell
cargo build --release --locked
.\target\release\dual-pane.exe
```

The first launch opens your user profile on the left and the working directory on the right. Later launches restore the open tabs and selected tab in each pane, saved folder pairs, the divider, and the theme from your Windows user settings. Pass up to two paths to override the active tabs for launch. Quote paths that contain spaces.

```powershell
.\target\release\dual-pane.exe "$env:USERPROFILE\Documents" "D:\Archive"
```

Add `--no-workspace` for a temporary session that starts with the defaults and leaves the saved workspace alone.

## Browse and organize files

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
| Ctrl+A | Select all visible rows in the active file list |
| Ctrl+F | Focus the active pane's filename filter |
| Alt+F7 | Find files in subfolders |
| Ctrl+1 / Ctrl+2 / Ctrl+3 | Sort by Name / Size / Modified. Repeat to reverse direction |
| Ctrl+Shift+C | Compare the folders in the two active tabs |
| Ctrl+J | Open the Transfers panel |
| Ctrl+P | Open the folder-pair picker |
| Ctrl+Shift+P | Save the two visible folders as a named pair |
| F2 | Rename the selected file or folder |
| Ctrl+M | Review a batch rename for the selected files |
| F3 | Preview the focused file in Quick View |
| F5 / F6 | Queue a confirmed copy / move to the opposite pane |
| F7 | Create a folder in the active pane |
| Delete | Confirm sending the selection to the Recycle Bin |
| Alt+V | Open the View menu |

You can double-click a row to open it, use Ctrl or Shift to select multiple rows, and drag the divider to resize the panes. The active pane is marked above its path field. The View menu offers System, Light, and Dark themes.

Each tab remembers its folder, sort, filter, scroll position, and focused item. A single selected item is restored when it is still visible. Reopening a tab reloads its folder. Each pane keeps at least one tab, supports up to 32 tabs, and keeps Back/Forward history for the current session.

Use **Pairs…** to preview the left and right paths for a saved pair. Opening a pair navigates the two active tabs and leaves the other tabs available. The picker also lets you save or remove a pair.

The lists start in folders-first natural name order. Click **Name**, **Size**, or **Modified** to sort and click the same column again to reverse the order. The filter matches any part of a filename and ignores case. New folder visits start with a clear filter. Back and Forward return to the filter for that visit. The pane footer shows the filtered count and the selected count. Rows hidden by a filter leave the selection.

Both local and UNC paths work for browsing. Dual Pane watches each open folder for changes and preserves the selected items and viewport when possible. An unsupported or disconnected watch leaves **Ctrl+R** available for a manual refresh. If Windows cannot recycle the selection, it presents a separate warning before a permanent deletion.

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

Open **Transfers** or press **Ctrl+J**. Copies, moves, and reviewed copies run in order, one job at a time. The job keeps the source names and destination captured at confirmation. Each source's metadata is checked before Windows starts the job.

The panel stays open while you browse. Select a job to see its paths, Windows' progress estimate, and results for the selected top-level items. Windows shows its file conflict and progress dialogs when needed. **Pause queue** holds the next job, **Remove waiting** cancels a job before it starts, and **Cancel transfer** asks Windows to stop the active job at its next cancellation point. Completed items remain in place.

**Retry verified** queues candidates from the selected job. At the front of the queue it checks the original regular file's identity and metadata and requires its destination name to be empty. Use **Open folders** to return both panes to the job's source and destination. Review unconfirmed items there before deciding whether to transfer them again.

The queue and its recent history belong to the current app session. Closing the app with waiting jobs asks whether to discard them.

## Implementation

Dual Pane uses Rust, Microsoft's `windows` crate, and Win32 common controls. Each pane uses one virtual list view and one folder watcher. Inactive tabs retain navigation state. Background folder enumeration uses a bounded work queue. Large listings show an early batch while the complete folder is read and sorted. Rows use shared Windows icons and format their metadata on demand. Filename filtering and sorting run on the same background workers. Folder watches use asynchronous Windows change notifications and group bursts of changes before rescanning. Copy, move, single-item rename, and recycle use Windows Shell file operations. Batch Rename uses native file handles and a virtual preview list.

```powershell
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

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
