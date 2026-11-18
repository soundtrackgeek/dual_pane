#![windows_subsystem = "windows"]

#[cfg(not(windows))]
compile_error!("Dual Pane requires Windows.");

#[cfg(windows)]
mod batch_rename;
#[cfg(windows)]
mod compare;
#[cfg(windows)]
mod directory;
#[cfg(windows)]
mod find;
#[cfg(windows)]
mod preview;
#[cfg(windows)]
mod shell;
#[cfg(windows)]
mod transfer;
#[cfg(windows)]
mod ui;
#[cfg(windows)]
mod watch;

#[cfg(windows)]
fn main() {
    if let Err(error) = ui::run() {
        ui::startup_error(&error.to_string());
    }
}
