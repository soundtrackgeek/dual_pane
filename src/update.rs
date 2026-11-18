use std::{ffi::c_void, ptr::null};

use semver::Version;
use serde_json::Value;
use windows::{
    Win32::Networking::WinHttp::*,
    core::{Error, PCWSTR, w},
};

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
pub const RELEASES: &str = "https://github.com/soundtrackgeek/dual_pane/releases";

pub fn check() -> Result<Option<Version>, String> {
    let current = Version::parse(CURRENT).map_err(|error| error.to_string())?;
    let releases = fetch_releases()?;
    newer_release(&current, &releases)
        .map_err(|_| "GitHub returned an invalid release list.".into())
}

fn newer_release(current: &Version, body: &[u8]) -> serde_json::Result<Option<Version>> {
    let releases: Vec<Value> = serde_json::from_slice(body)?;
    Ok(releases
        .iter()
        .filter(|release| release["draft"].as_bool() == Some(false))
        .filter_map(|release| {
            let tag = release["tag_name"].as_str()?.strip_prefix('v')?;
            let version = Version::parse(tag).ok()?;
            if current.pre.is_empty()
                && (!version.pre.is_empty() || release["prerelease"].as_bool() != Some(false))
            {
                return None;
            }
            let assets = release["assets"].as_array()?;
            let prefix = format!("filehelm-{version}-windows-x64");
            let packaged = [format!("{prefix}.msi"), format!("{prefix}-portable.zip")]
                .iter()
                .all(|filename| {
                    assets.iter().any(|asset| {
                        asset["name"].as_str() == Some(filename)
                            && asset["state"].as_str() == Some("uploaded")
                    })
                });
            (packaged && version.cmp_precedence(current).is_gt()).then_some(version)
        })
        .max_by(Version::cmp_precedence))
}

struct InternetHandle(*mut c_void);

impl InternetHandle {
    fn new(handle: *mut c_void) -> windows::core::Result<Self> {
        if handle.is_null() {
            Err(Error::from_thread())
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for InternetHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

fn fetch_releases() -> Result<Vec<u8>, String> {
    const MAX_RESPONSE: usize = 2 * 1024 * 1024;
    let (status, body) = (|| -> windows::core::Result<_> {
        unsafe {
            let session = InternetHandle::new(WinHttpOpen(
                w!("Filehelm update check"),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ))?;
            WinHttpSetTimeouts(session.0, 5_000, 5_000, 5_000, 5_000)?;
            let host =
                InternetHandle::new(WinHttpConnect(session.0, w!("api.github.com"), 443, 0))?;
            let request = InternetHandle::new(WinHttpOpenRequest(
                host.0,
                w!("GET"),
                w!("/repos/soundtrackgeek/dual_pane/releases?per_page=100"),
                PCWSTR::null(),
                PCWSTR::null(),
                null(),
                WINHTTP_FLAG_SECURE,
            ))?;
            WinHttpSetOption(
                Some(request.0),
                WINHTTP_OPTION_REDIRECT_POLICY,
                Some(&WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_ne_bytes()),
            )?;
            WinHttpSendRequest(request.0, None, None, 0, 0, 0)?;
            WinHttpReceiveResponse(request.0, std::ptr::null_mut())?;
            let mut status = 0_u32;
            let mut size = size_of::<u32>() as u32;
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some((&mut status as *mut u32).cast()),
                &mut size,
                std::ptr::null_mut(),
            )?;
            let mut body = Vec::new();
            if status == 200 {
                let mut buffer = [0_u8; 8192];
                let mut read = 0;
                while body.len() <= MAX_RESPONSE {
                    WinHttpReadData(
                        request.0,
                        buffer.as_mut_ptr().cast(),
                        buffer.len() as u32,
                        &mut read,
                    )?;
                    if read == 0 {
                        break;
                    }
                    body.extend_from_slice(&buffer[..read as usize]);
                }
            }
            Ok((status, body))
        }
    })()
    .map_err(|error| error.message())?;

    match status {
        200 if body.len() <= MAX_RESPONSE => Ok(body),
        200 => Err("GitHub returned an unexpectedly large release list.".into()),
        403 | 429 => Err("GitHub refused the update check. Try again later.".into()),
        _ => Err(format!("GitHub returned HTTP {status}.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release(tag: &str, draft: bool, prerelease: bool, packaged: bool) -> Value {
        let prefix = format!("filehelm-{}-windows-x64", tag.trim_start_matches('v'));
        let assets = if packaged {
            json!([
                {"name": format!("{prefix}.msi"), "state": "uploaded"},
                {"name": format!("{prefix}-portable.zip"), "state": "uploaded"},
            ])
        } else {
            json!([])
        };
        json!({"tag_name":tag, "draft":draft, "prerelease":prerelease, "assets":assets})
    }

    #[test]
    fn picks_newer_packaged_releases_for_the_installed_channel() {
        let releases = json!([
            release("v1.1.0-rc.9", false, true, true),
            release("v8.0.0", true, false, true),
            release("v7.0.0", false, false, false),
            release("v1.0.0", false, false, true),
            release("v1.1.0-rc.10", false, true, true),
            release("vnot-a-version", false, false, true),
        ]);
        let body = serde_json::to_vec(&releases).unwrap();
        let find = |current| newer_release(&Version::parse(current).unwrap(), &body).unwrap();
        assert_eq!(
            find("1.1.0-rc.9"),
            Some(Version::parse("1.1.0-rc.10").unwrap())
        );
        assert_eq!(find("1.0.0"), None);
        assert_eq!(find("0.11.0"), Some(Version::parse("1.0.0").unwrap()));
        assert_eq!(find("1.1.0-rc.10"), None);

        let stable = serde_json::to_vec(&[release("v1.1.0", false, false, true)]).unwrap();
        assert_eq!(
            newer_release(&Version::parse("1.1.0-rc.10").unwrap(), &stable).unwrap(),
            Some(Version::parse("1.1.0").unwrap())
        );
        assert!(newer_release(&Version::parse(CURRENT).unwrap(), b"{}").is_err());
    }
}
