//! Quiet, advisory public release discovery. No artifact download or installation.
//! The website contract is `downloads/latest.json`, schema 1. Public releases
//! use canonical SemVer: a new binary under the same version is not an update.

use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

pub const CATALOG_URL: &str = "https://erlkonig.ai/downloads/latest.json";
pub const DOWNLOAD_URL: &str = "https://erlkonig.ai/#downloads";
pub const MAX_CATALOG_BYTES: usize = 64 * 1024;
const REPOSITORY: &str = "erlkonig-ai/erlkonig-app";

/// Only stable releases participate. Prerelease/local builds deliberately do
/// not receive stable-channel notices; build metadata has no ordering here.
pub fn stable_version(value: &str) -> Option<[u64; 3]> {
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut version = [0; 3];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty()
            || !part.bytes().all(|b| b.is_ascii_digit())
            || (part.len() > 1 && part.starts_with('0'))
        {
            return None;
        }
        version[index] = part.parse().ok()?;
    }
    Some(version)
}

pub fn target() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("aarch64-apple-darwin")
    } else if cfg!(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "gnu"
    )) {
        Some("x86_64-unknown-linux-gnu")
    } else if cfg!(all(
        target_os = "linux",
        target_arch = "aarch64",
        target_env = "gnu"
    )) {
        Some("aarch64-unknown-linux-gnu")
    } else {
        None
    }
}

/// Read only the catalog's selected stable release, matching the website.
/// Unknown fields are fine; malformed or contradictory selected identity is
/// ignored. This is a notice, not an independent artifact verification system.
pub fn offered_version(bytes: &[u8], current: &str, target: &str) -> Option<String> {
    let current = stable_version(current)?;
    if bytes.len() > MAX_CATALOG_BYTES {
        return None;
    }
    let catalog: Value = serde_json::from_slice(bytes).ok()?;
    if catalog["schema"].as_u64()? != 1 || catalog["repository"] != REPOSITORY {
        return None;
    }
    let selected = catalog["default_release_id"]
        .as_u64()
        .filter(|id| *id > 0)?;
    let mut matches = catalog["releases"]
        .as_array()?
        .iter()
        .filter(|entry| entry["release"]["id"].as_u64() == Some(selected));
    let entry = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let release = &entry["release"];
    let manifest = &entry["manifest"];
    let version = manifest["version"].as_str()?;
    if stable_version(version)? <= current
        || release["draft"] != false
        || release["prerelease"] != false
        || release["tag_name"] != format!("v{version}")
        || manifest["schema"].as_u64()? != 1
        || manifest["repository"] != REPOSITORY
        || manifest["release_id"].as_u64()? != selected
    {
        return None;
    }
    let supported = manifest["artifacts"].as_array()?.iter().any(|a| {
        a["target"] == target
            && a["size"].as_u64().is_some_and(|n| n > 0)
            && match target {
                "aarch64-apple-darwin" => {
                    a["format"] == "dmg" && a["signing"] == "developer-id-notarized"
                }
                "x86_64-unknown-linux-gnu" | "aarch64-unknown-linux-gnu" => {
                    a["format"] == "AppImage" && a["signing"] == "unsigned"
                },
                _ => false,
            }
    });
    supported.then(|| version.to_owned())
}

/// Called only by the app's background worker. Cancellation drops the request;
/// response bodies, errors and machine/account identifiers are never logged.
pub fn fetch(cancel: Arc<AtomicBool>) -> Option<Vec<u8>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    let result = runtime.block_on(async {
        let client = reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
            .build()
            .ok()?;
        let request = async {
            let mut response = client.get(CATALOG_URL).send().await.ok()?;
            if response.status() != reqwest::StatusCode::OK
                || response
                    .content_length()
                    .is_some_and(|n| n > MAX_CATALOG_BYTES as u64)
            {
                return None;
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.ok()? {
                if bytes.len().checked_add(chunk.len())? > MAX_CATALOG_BYTES {
                    return None;
                }
                bytes.extend_from_slice(&chunk);
            }
            Some(bytes)
        };
        let cancelled = async {
            while !cancel.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        tokio::select! {
            biased;
            _ = cancelled => None,
            result = request => result,
        }
    });
    // DNS may use Tokio's blocking pool: quitting must not wait for an OS DNS
    // resolver beyond the request/cancellation deadline.
    runtime.shutdown_background();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog(version: &str) -> Value {
        json!({"schema":1,"repository":REPOSITORY,"default_release_id":12,"releases":[{
            "release":{"id":12,"draft":false,"prerelease":false,"tag_name":format!("v{version}")},
            "manifest":{"schema":1,"repository":REPOSITORY,"release_id":12,"version":version,
                "artifacts":[{"target":"aarch64-apple-darwin","format":"dmg","size":123,"signing":"developer-id-notarized"}]}
        }]})
    }
    fn offer(value: &Value, current: &str) -> Option<String> {
        offered_version(
            &serde_json::to_vec(value).unwrap(),
            current,
            "aarch64-apple-darwin",
        )
    }
    #[test]
    fn accepts_the_site_generators_exact_synthetic_stable_feed() {
        // Produced by the website's validated catalog renderer, not hand-edited.
        let bytes = include_bytes!("../tests/fixtures/update-latest.json");
        for target in ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
            assert_eq!(
                offered_version(bytes, "0.1.1", target),
                Some("1.3.0".into())
            );
            assert_eq!(offered_version(bytes, "1.3.0", target), None);
        }
    }

    #[test]
    fn linux_appimage_notice_never_substitutes_architecture_or_tarball() {
        let mut value = catalog("0.2.0");
        value["releases"][0]["manifest"]["artifacts"] = json!([{
            "target": "aarch64-unknown-linux-gnu", "format": "AppImage",
            "size": 123, "signing": "unsigned"
        }]);
        let bytes = serde_json::to_vec(&value).unwrap();
        assert_eq!(offered_version(&bytes, "0.1.1", "aarch64-unknown-linux-gnu"), Some("0.2.0".into()));
        assert_eq!(offered_version(&bytes, "0.1.1", "x86_64-unknown-linux-gnu"), None);
        value["releases"][0]["manifest"]["artifacts"][0]["format"] = json!("tar.gz");
        assert_eq!(offered_version(&serde_json::to_vec(&value).unwrap(), "0.1.1", "aarch64-unknown-linux-gnu"), None);
    }

    #[test]
    fn stable_order_is_numeric_and_requires_public_version_bump() {
        assert_eq!(offer(&catalog("0.1.10"), "0.1.9"), Some("0.1.10".into()));
        assert_eq!(offer(&catalog("0.1.1"), "0.1.1"), None);
        assert_eq!(offer(&catalog("0.1.1"), "0.2.0"), None);
        for value in [
            "0.1.2-rc.1",
            "0.1.2+42",
            "00.1.2",
            "0.1",
            "0.1.2.3",
            "0.1.18446744073709551616",
        ] {
            assert_eq!(offer(&catalog(value), "0.1.1"), None);
            assert_eq!(offer(&catalog("1.0.0"), value), None);
        }
    }
    #[test]
    fn ignores_unselected_and_ineligible_releases() {
        let mut value = catalog("0.2.0");
        value["releases"]
            .as_array_mut()
            .unwrap()
            .push(catalog("9.0.0")["releases"][0].clone());
        assert_eq!(offer(&value, "0.1.1"), None, "duplicate selected identity");
        for (pointer, replacement) in [
            ("/schema", json!(2)),
            ("/repository", json!("other/app")),
            ("/default_release_id", Value::Null),
            ("/releases/0/release/draft", json!(true)),
            ("/releases/0/release/prerelease", json!(true)),
            ("/releases/0/manifest/release_id", json!(99)),
            (
                "/releases/0/manifest/artifacts/0/signing",
                json!("development-ad-hoc"),
            ),
            (
                "/releases/0/manifest/artifacts/0/target",
                json!("x86_64-unknown-linux-gnu"),
            ),
        ] {
            let mut value = catalog("0.2.0");
            *value.pointer_mut(pointer).unwrap() = replacement;
            assert_eq!(offer(&value, "0.1.1"), None, "{pointer}");
        }
    }
    #[test]
    fn bad_and_oversized_bodies_are_quiet() {
        for bytes in [
            b"<html>error</html>".as_slice(),
            b"{}",
            &vec![b' '; MAX_CATALOG_BYTES + 1],
        ] {
            assert_eq!(
                offered_version(bytes, "0.1.1", "aarch64-apple-darwin"),
                None
            );
        }
    }
    #[test]
    fn linux_requires_matching_artifact_and_never_uses_catalog_links() {
        let mut value = catalog("0.2.0");
        value["releases"][0]["release"]["html_url"] = json!("https://evil.invalid/");
        value["releases"][0]["manifest"]["artifacts"] = json!([
            {"target":"x86_64-unknown-linux-gnu","size":42,"format":"AppImage","signing":"unsigned"}]);
        let bytes = serde_json::to_vec(&value).unwrap();
        assert_eq!(
            offered_version(&bytes, "0.1.1", "x86_64-unknown-linux-gnu"),
            Some("0.2.0".into())
        );
        assert_eq!(
            offered_version(&bytes, "0.1.1", "aarch64-unknown-linux-gnu"),
            None
        );
        assert_eq!(DOWNLOAD_URL, "https://erlkonig.ai/#downloads");
    }
}
