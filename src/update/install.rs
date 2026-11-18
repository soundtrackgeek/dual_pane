use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read},
    os::windows::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{self, Command},
    thread,
    time::Duration,
};

use tempfile::{Builder, TempDir};
use windows::Win32::{
    Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0},
    System::{Registry::*, Threading::*},
    UI::WindowsAndMessaging::{MB_ICONWARNING, MB_OK, MessageBoxW},
};

use super::*;
use crate::directory::wide;

const PREFIX: &str = "filehelm-update-";
const HELPER: &str = "filehelm-updater.exe";
const INSTALLER: &str = "update.msi";
const PORTABLE: &str = "update.zip";
const HANDOFF: &str = "--apply-filehelm-update";
const CLEANUP: &str = "FILEHELM_UPDATE_CLEANUP";

pub struct Prepared {
    stage: TempDir,
    target: PathBuf,
    digest: String,
}

impl Release {
    pub fn download(self) -> Result<Prepared, String> {
        let target =
            fs::canonicalize(std::env::current_exe().map_err(display)?).map_err(display)?;
        let (asset, package) = if installed(&target) {
            (self.msi, INSTALLER)
        } else {
            (self.portable, PORTABLE)
        };
        let path = wide(format!(
            "/soundtrackgeek/dual_pane/releases/download/v{}/{}",
            self.version, asset.name
        ));
        let bytes = fetch(w!("github.com"), PCWSTR(path.as_ptr()), asset.size, true)?;
        asset.verify(&bytes)?;
        let stage = Builder::new().prefix(PREFIX).tempdir().map_err(display)?;
        fs::write(stage.path().join(package), &bytes).map_err(display)?;
        fs::copy(&target, stage.path().join(HELPER)).map_err(display)?;
        Ok(Prepared {
            stage,
            target,
            digest: asset.digest,
        })
    }
}

impl Prepared {
    pub fn hand_off(self, temporary_paths: Option<[PathBuf; 2]>) -> Result<(), String> {
        let mut helper = Command::new(self.stage.path().join(HELPER));
        helper
            .arg(HANDOFF)
            .arg(process::id().to_string())
            .arg(&self.target)
            .arg(&self.digest)
            .env_remove(CLEANUP);
        if let Some(paths) = temporary_paths {
            helper.args(["--no-workspace", "--"]).args(paths);
        }
        helper.spawn().map_err(display)?;
        let _ = self.stage.keep();
        Ok(())
    }
}

fn display(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn installed(target: &Path) -> bool {
    let mut data = [0_u16; 32768];
    let mut bytes = size_of_val(&data) as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Filehelm\\Installation"),
            w!("Directory"),
            RRF_RT_REG_SZ,
            None,
            Some(data.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    if result.is_err() {
        return false;
    }
    use std::os::windows::ffi::OsStringExt;
    let end = data
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(data.len());
    let path = PathBuf::from(OsString::from_wide(&data[..end])).join("filehelm.exe");
    fs::canonicalize(path)
        .ok()
        .zip(fs::canonicalize(target).ok())
        .is_some_and(|(registered, running)| registered == running)
}

/// Returns true when this process is the staged installer helper.
pub fn startup() -> bool {
    if let Ok(executable) = std::env::current_exe()
        && executable.file_name() == Some(OsStr::new(HELPER))
        && let Some(stage) = executable.parent().filter(|path| is_stage(path))
    {
        let mut args = std::env::args_os().skip(1);
        if args.next().as_deref() == Some(OsStr::new(HANDOFF)) {
            let parent = args
                .next()
                .and_then(|pid| pid.to_str()?.parse::<u32>().ok());
            if let (Some(parent), Some(target), Some(digest)) = (parent, args.next(), args.next()) {
                apply_and_restart(
                    parent,
                    &executable,
                    stage,
                    &PathBuf::from(target),
                    &digest,
                    args,
                );
            }
            return true;
        }
    }
    if let Some(stage) = std::env::var_os(CLEANUP)
        .map(PathBuf::from)
        .filter(|path| is_stage(path))
    {
        thread::spawn(move || {
            // Windows releases the helper's executable mapping after it exits.
            for _ in 0..40 {
                clean(&stage);
                if fs::remove_dir(&stage).is_ok() || !stage.exists() {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
        });
    }
    false
}

fn is_stage(path: &Path) -> bool {
    let name = path.file_name().and_then(OsStr::to_str);
    if !name.is_some_and(|name| name.starts_with(PREFIX)) {
        return false;
    }
    let Ok(temp) = fs::canonicalize(std::env::temp_dir()) else {
        return false;
    };
    path.parent()
        .and_then(|parent| fs::canonicalize(parent).ok())
        .as_ref()
        == Some(&temp)
        && fs::symlink_metadata(path).is_ok_and(|info| {
            use std::os::windows::fs::MetadataExt;
            use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
            info.is_dir() && info.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0
        })
}

fn clean(stage: &Path) {
    for name in [HELPER, INSTALLER, PORTABLE] {
        let _ = fs::remove_file(stage.join(name));
    }
}

fn wait_for_exit(pid: u32) -> Result<(), String> {
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) };
    match handle {
        Ok(handle) => {
            let result = unsafe { WaitForSingleObject(handle, 30_000) };
            unsafe {
                let _ = CloseHandle(handle);
            }
            if result != WAIT_OBJECT_0 {
                return Err("Filehelm did not close in time. Try Update again.".into());
            }
        }
        Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => {}
        Err(error) => return Err(error.message()),
    }
    Ok(())
}

fn apply_and_restart(
    parent: u32,
    helper: &Path,
    stage: &Path,
    target: &Path,
    digest: &OsStr,
    args: impl Iterator<Item = OsString>,
) {
    if let Err(error) = wait_for_exit(parent) {
        clean(stage);
        report(&error);
        return;
    }
    let mut restart = Command::new(target);
    restart.args(args).env(CLEANUP, stage);
    let result = apply(helper, stage, target, digest, &mut restart);
    if let Err(error) = result {
        report(&error);
        if let Err(error) = restart.spawn() {
            report(&format!(
                "Could not reopen Filehelm.\n\n{}\n\n{error}",
                target.display()
            ));
            clean(stage);
        }
    }
}

fn apply(
    helper: &Path,
    stage: &Path,
    target: &Path,
    digest: &OsStr,
    restart: &mut Command,
) -> Result<(), String> {
    // Serialize helper processes for the current user and reject stale updates.
    let _lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(std::env::temp_dir().join("filehelm-update.lock"))
        .map_err(|error| format!("Could not obtain the update lock. {error}"))?;
    if !target.is_absolute()
        || fs::read(helper).map_err(display)? != fs::read(target).map_err(display)?
    {
        return Err("The running copy of Filehelm changed before installation.".into());
    }
    let is_installed = installed(target);
    let package = stage.join(if is_installed { INSTALLER } else { PORTABLE });
    let mut bytes = Vec::new();
    File::open(&package)
        .map_err(display)?
        .take(MAX_PACKAGE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(display)?;
    if bytes.len() > MAX_PACKAGE || digest != OsStr::new(&format!("{:x}", Sha256::digest(&bytes))) {
        return Err("The staged update failed its SHA-256 integrity check.".into());
    }
    drop(bytes);
    if is_installed {
        install_msi(&package)?;
        restart.spawn().map_err(display)?;
    } else {
        replace_portable(&package, target, restart)?;
    }
    Ok(())
}

fn install_msi(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut system = [0_u16; 32768];
    let length = unsafe { GetSystemDirectoryW(Some(&mut system)) } as usize;
    if length == 0 || length >= system.len() {
        return Err("Could not locate Windows Installer.".into());
    }
    let exe = PathBuf::from(OsString::from_wide(&system[..length])).join("msiexec.exe");
    let status = Command::new(exe)
        .arg("/i")
        .arg(path)
        .args([
            "/passive",
            "/norestart",
            "REBOOT=ReallySuppress",
            "MSIRESTARTMANAGERCONTROL=Disable",
            "MSIDISABLERMRESTART=1",
        ])
        .creation_flags(CREATE_NO_WINDOW.0)
        .status()
        .map_err(display)?;
    match status.code() {
        Some(0) => Ok(()),
        Some(3010 | 1641) => Err("Windows Installer needs a restart to finish updating. Save your work and restart Windows when convenient.".into()),
        _ => Err(format!("Windows Installer could not apply the update ({status}).")),
    }
}

fn replace_portable(package: &Path, target: &Path, restart: &mut Command) -> Result<(), String> {
    let parent = target
        .parent()
        .ok_or("Could not locate the portable folder.")?;
    let replace = Builder::new()
        .prefix(PREFIX)
        .tempdir_in(parent)
        .map_err(display)?;
    let next = replace.path().join("new.exe");
    let backup = replace.path().join("previous.exe");
    extract_executable(package, &next).map_err(display)?;
    fs::copy(target, &backup).map_err(display)?;
    fs::rename(&next, target).map_err(|error| {
        format!("Could not replace the portable executable. Close other copies of Filehelm and try again.\n\n{error}")
    })?;
    if let Err(error) = restart.spawn() {
        if let Err(rollback) = fs::rename(&backup, target) {
            let backup = replace.keep().join("previous.exe");
            return Err(format!(
                "Could not start the update or restore the previous copy. The backup is at {}.\n\n{error}\n{rollback}",
                backup.display()
            ));
        }
        return Err(format!(
            "Could not start the updated executable. The previous copy was restored.\n\n{error}"
        ));
    }
    Ok(())
}

fn extract_executable(package: &Path, destination: &Path) -> io::Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(package)?)?;
    let entry = archive.by_name("filehelm.exe")?;
    let size = entry.size();
    if !entry.is_file() || size == 0 || size > MAX_PACKAGE as u64 {
        return Err(io::Error::other(
            "The portable archive contains an invalid executable.",
        ));
    }
    let mut file = File::create(destination)?;
    if io::copy(&mut entry.take(size + 1), &mut file)? != size {
        return Err(io::Error::other("The portable executable is incomplete."));
    }
    file.sync_all()
}

fn report(error: &str) {
    let text = wide(error);
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            w!("Filehelm update"),
            MB_OK | MB_ICONWARNING,
        );
    }
}
