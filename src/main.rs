#![windows_subsystem = "windows"]

#[cfg(not(windows))]
compile_error!("Filehelm requires Windows.");

#[cfg(windows)]
mod archive;
#[cfg(windows)]
mod batch_rename;
#[cfg(windows)]
mod compare;
#[cfg(windows)]
mod directory;
#[cfg(windows)]
mod file_data;
#[cfg(windows)]
mod find;
#[cfg(windows)]
mod folder_size;
#[cfg(windows)]
mod preview;
#[cfg(windows)]
mod shell;
#[cfg(windows)]
mod transfer;
#[cfg(windows)]
mod ui;
#[cfg(windows)]
mod undo;
#[cfg(windows)]
mod update;
#[cfg(windows)]
mod watch;

#[cfg(windows)]
fn main() {
    if update::startup() {
        return;
    }
    if let Err(error) = ui::run() {
        ui::startup_error(&error.to_string());
    }
}
