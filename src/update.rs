use std::{ffi::c_void, ptr::null};

use semver::Version;
use serde_json::Value;
use sha2::{Digest, Sha256};
use windows::{
    Win32::Networking::WinHttp::*,
    core::{Error, PCWSTR, w},
};

mod install;
pub use install::{Prepared, startup};

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
pub const RELEASES: &str = "https://github.com/soundtrackgeek/dual_pane/releases";
const MAX_PACKAGE: usize = 64 * 1024 * 1024;

#[derive(Debug)]
pub struct Release {
    pub version: Version,
    msi: Asset,
    portable: Asset,
}

#[derive(Debug)]
struct Asset {
    name: String,
    size: usize,
    digest: String,
}

impl Asset {
    fn from_release(assets: &[Value], name: String) -> Option<Self> {
        let asset = assets.iter().find(|asset| {
            asset["name"].as_str() == Some(&name) && asset["state"].as_str() == Some("uploaded")
        })?;
        let size = usize::try_from(asset["size"].as_u64()?).ok()?;
        let digest = asset["digest"].as_str()?.strip_prefix("sha256:")?;
        (size > 0
            && size <= MAX_PACKAGE
            && digest.len() == 64
            && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| Self {
            name,
            size,
            digest: digest.to_ascii_lowercase(),
        })
    }

    fn verify(&self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() == self.size && format!("{:x}", Sha256::digest(bytes)) == self.digest {
            Ok(())
        } else {
            Err("The downloaded package failed its SHA-256 integrity check.".into())
        }
    }
}

pub fn check() -> Result<Option<Release>, String> {
    let current = Version::parse(CURRENT).map_err(|error| error.to_string())?;
    let releases = fetch(
        w!("api.github.com"),
        w!("/repos/soundtrackgeek/dual_pane/releases?per_page=100"),
        2 * 1024 * 1024,
        false,
    )?;
    newer_release(&current, &releases)
        .map_err(|_| "GitHub returned an invalid release list.".into())
}

fn newer_release(current: &Version, body: &[u8]) -> serde_json::Result<Option<Release>> {
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
            let msi = Asset::from_release(assets, format!("{prefix}.msi"))?;
            let portable = Asset::from_release(assets, format!("{prefix}-portable.zip"))?;
            version.cmp_precedence(current).is_gt().then_some(Release {
                version,
                msi,
                portable,
            })
        })
        .max_by(|a, b| a.version.cmp_precedence(&b.version)))
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

fn fetch(host: PCWSTR, path: PCWSTR, limit: usize, redirect: bool) -> Result<Vec<u8>, String> {
    let (status, body) = (|| -> windows::core::Result<_> {
        unsafe {
            let session = InternetHandle::new(WinHttpOpen(
                w!("Filehelm updater"),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ))?;
            WinHttpSetTimeouts(session.0, 5_000, 5_000, 5_000, 5_000)?;
            let host = InternetHandle::new(WinHttpConnect(session.0, host, 443, 0))?;
            let request = InternetHandle::new(WinHttpOpenRequest(
                host.0,
                w!("GET"),
                path,
                PCWSTR::null(),
                PCWSTR::null(),
                null(),
                WINHTTP_FLAG_SECURE,
            ))?;
            let policy = if redirect {
                WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP
            } else {
                WINHTTP_OPTION_REDIRECT_POLICY_NEVER
            };
            WinHttpSetOption(
                Some(request.0),
                WINHTTP_OPTION_REDIRECT_POLICY,
                Some(&policy.to_ne_bytes()),
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
                while body.len() <= limit {
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
        200 if body.len() <= limit => Ok(body),
        200 => Err("GitHub returned more data than expected.".into()),
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
            let digest = format!("sha256:{:x}", Sha256::digest(b"package"));
            json!([
                {"name": format!("{prefix}.msi"), "state": "uploaded", "size": 7, "digest": digest},
                {"name": format!("{prefix}-portable.zip"), "state": "uploaded", "size": 7, "digest": digest},
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
        let find = |current| {
            newer_release(&Version::parse(current).unwrap(), &body)
                .unwrap()
                .map(|release| release.version)
        };
        assert_eq!(
            find("1.1.0-rc.9"),
            Some(Version::parse("1.1.0-rc.10").unwrap())
        );
        assert_eq!(find("1.0.0"), None);
        assert_eq!(find("0.11.0"), Some(Version::parse("1.0.0").unwrap()));
        assert_eq!(find("1.1.0-rc.10"), None);

        let stable = serde_json::to_vec(&[release("v1.1.0", false, false, true)]).unwrap();
        assert_eq!(
            newer_release(&Version::parse("1.1.0-rc.10").unwrap(), &stable)
                .unwrap()
                .map(|release| release.version),
            Some(Version::parse("1.1.0").unwrap())
        );
        assert!(newer_release(&Version::parse(CURRENT).unwrap(), b"{}").is_err());
    }

    #[test]
    fn rejects_truncated_and_modified_downloads() {
        let candidate = release("v2.0.0", false, false, true);
        let asset = Asset::from_release(
            candidate["assets"].as_array().unwrap(),
            "filehelm-2.0.0-windows-x64.msi".into(),
        )
        .unwrap();
        assert!(asset.verify(b"package").is_ok());
        assert!(asset.verify(b"packag").is_err());
        assert!(asset.verify(b"Package").is_err());
    }
}
