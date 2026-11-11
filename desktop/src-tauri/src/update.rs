//! Opt-in update check: asks GitHub for the latest release's version number. Nothing is installed.
use crate::stt::HTTP;
use crate::App;
use anyhow::{anyhow, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;

/// The same repository `install.sh` downloads from.
const REPO: &str = "mtr1920/opstrellis";

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current: String,
    /// The release tag as published, e.g. "v0.3.3".
    pub latest: String,
    pub newer: bool,
    /// Built here from the tag, never taken from GitHub's reply.
    pub url: String,
}

/// `v1.2.3` or `1.2.3` (ASCII digits only); anything else is refused, since a tag is untrusted input.
fn parse_tag(tag: &str) -> Option<(u64, u64, u64)> {
    let mut parts = tag.strip_prefix('v').unwrap_or(tag).split('.');
    let mut next = || {
        let part = parts.next()?;
        (!part.is_empty() && part.len() <= 9 && part.bytes().all(|b| b.is_ascii_digit())).then(|| part.parse::<u64>().ok()).flatten()
    };
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

pub fn release_url(tag: &str) -> Result<String> {
    ensure!(parse_tag(tag).is_some(), "Not a release version");
    Ok(format!("https://github.com/{REPO}/releases/tag/{tag}"))
}

fn parse_release(body: &str, current: &str) -> Result<UpdateInfo> {
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
    }
    let release: Release = serde_json::from_str(body).context("Unexpected reply from GitHub")?;
    let latest = parse_tag(&release.tag_name).ok_or_else(|| anyhow!("GitHub's latest release has an unexpected version"))?;
    let installed = parse_tag(current).ok_or_else(|| anyhow!("This app's version isn't a plain version number"))?;
    Ok(UpdateInfo { current: current.to_string(), newer: latest > installed, url: release_url(&release.tag_name)?, latest: release.tag_name })
}

pub async fn check(current: &str) -> Result<UpdateInfo> {
    let resp = HTTP
        .get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("User-Agent", "opstrellis-desktop")
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .context("Couldn't reach github.com")?;
    ensure!(resp.status().is_success(), "GitHub answered {}", resp.status());
    parse_release(&resp.text().await?, current)
}

const DAY_SECS: i64 = 24 * 3600;
const RETRY_SECS: i64 = 3600;

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Due when never checked or the last check was a day ago (also if the clock was set back), and
/// not before `retry_at`. Wall-clock time, so sleep and restarts count.
fn is_due(now: i64, last_checked: Option<i64>, retry_at: i64) -> bool {
    now >= retry_at && last_checked.is_none_or(|t| t > now || now - t >= DAY_SECS)
}

/// While "Check for updates automatically" is on: a check whenever the last successful one is over
/// a day old (the first tick after start if so), and an hour after a failed try. The setting is
/// read on every tick, so changing it needs no restart; when it is off nothing is sent.
pub fn spawn_loop(app: Arc<App>) {
    tauri::async_runtime::spawn(async move {
        let current = app.handle.package_info().version.to_string();
        let mut retry_at = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let s = app.settings();
            let now = now_secs();
            if !s.check_updates || !is_due(now, s.update_checked_at, retry_at) {
                continue;
            }
            match check(&current).await {
                Ok(info) => {
                    if let Err(e) = app.update_settings(|s| s.update_checked_at = Some(now)) {
                        log::debug!("couldn't record the update check: {e:#}");
                        retry_at = now + RETRY_SECS;
                    }
                    if info.newer {
                        let _ = app.handle.emit("update-available", info);
                    }
                }
                Err(e) => {
                    log::debug!("update check failed: {e:#}");
                    retry_at = now + RETRY_SECS;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn newer(latest: &str, current: &str) -> bool {
        parse_tag(latest).unwrap() > parse_tag(current).unwrap()
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(newer("1.10.0", "1.9.9"));
        assert!(newer("v0.3.3", "0.3.2"));
        assert!(newer("0.4.0", "0.3.99"));
        assert!(newer("2.0.0", "1.99.99"));
        assert!(!newer("0.3.2", "0.3.2"));
        assert!(!newer("v0.3.1", "0.3.2"));
        assert!(!newer("1.9.9", "1.10.0"));
    }

    #[test]
    fn only_plain_release_tags_are_accepted() {
        for ok in ["1.2.3", "v1.2.3", "v0.0.0", "10.20.30", "v123456789.0.0"] {
            assert!(parse_tag(ok).is_some(), "{ok}");
        }
        for bad in [
            "v1.2", "1.2", "1.2.3-beta", "v1.2.3-rc.1", "1.2.3.4", "../x", "", "v", "vv1.2.3", "V1.2.3", "1..3", "1.2.", " 1.2.3", "1.2.3\n",
            "https://evil.example/1.2.3", "1.2.3/../../x", "1.2.3?x=1", "1.2.\u{663}", "+1.2.3", "-1.2.3", "1.2.1234567890", "latest",
        ] {
            assert!(parse_tag(bad).is_none(), "{bad:?}");
            assert!(release_url(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn url_is_built_here() {
        assert_eq!(release_url("v0.3.3").unwrap(), "https://github.com/mtr1920/opstrellis/releases/tag/v0.3.3");
        assert_eq!(release_url("1.0.0").unwrap(), "https://github.com/mtr1920/opstrellis/releases/tag/1.0.0");
    }

    const FIXTURE: &str = r#"{
        "url": "https://api.github.com/repos/mtr1920/opstrellis/releases/1",
        "html_url": "https://evil.example/phish",
        "tag_name": "v0.4.0",
        "name": "OpsTrellis 0.4.0",
        "draft": false,
        "prerelease": false,
        "assets": [{ "name": "opstrellis-macos-arm64.app.tar.gz", "browser_download_url": "https://evil.example/a.tar.gz" }],
        "body": "Notes"
    }"#;

    #[test]
    fn release_reply_is_parsed_from_the_tag_only() {
        let info = parse_release(FIXTURE, "0.3.2").unwrap();
        assert_eq!(
            info,
            UpdateInfo {
                current: "0.3.2".into(),
                latest: "v0.4.0".into(),
                newer: true,
                url: "https://github.com/mtr1920/opstrellis/releases/tag/v0.4.0".into()
            }
        );
        assert!(!parse_release(FIXTURE, "0.4.0").unwrap().newer);
        assert!(!parse_release(FIXTURE, "1.0.0").unwrap().newer);
    }

    #[test]
    fn unexpected_replies_are_errors() {
        for body in [r#"{ "tag_name": "v1.2" }"#, r#"{ "tag_name": "https://evil.example" }"#, r#"{ "tag_name": 5 }"#, r#"{ "message": "Not Found" }"#, "[]", "", "<html>"] {
            assert!(parse_release(body, "0.3.2").is_err(), "{body}");
        }
        assert!(parse_release(FIXTURE, "dev").is_err());
    }

    #[test]
    fn a_check_is_due_once_a_day_by_the_wall_clock() {
        let now = 1_800_000_000;
        assert!(is_due(now, None, 0), "never checked");
        assert!(!is_due(now, Some(now - DAY_SECS + 1), 0), "checked less than a day ago, even right after a restart");
        assert!(is_due(now, Some(now - DAY_SECS), 0));
        assert!(is_due(now, Some(now + 5), 0), "the clock was set back");
        assert!(!is_due(now, None, now + 1), "a failed try waits an hour");
        assert!(is_due(now, None, now));
    }

    #[test]
    fn info_serializes_for_the_ui() {
        let v = serde_json::to_value(parse_release(FIXTURE, "0.3.2").unwrap()).unwrap();
        assert_eq!(v, serde_json::json!({ "current": "0.3.2", "latest": "v0.4.0", "newer": true, "url": "https://github.com/mtr1920/opstrellis/releases/tag/v0.4.0" }));
    }
}
