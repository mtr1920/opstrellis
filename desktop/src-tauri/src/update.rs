//! Opt-in update check: asks GitHub for the latest release's version number; it installs nothing.
//! Only when the user clicks to, the update is downloaded (its signature checked by the updater
//! plugin) and then installed on restart.
use crate::stt::HTTP;
use crate::App;
use anyhow::{anyhow, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::{Update, UpdaterExt};

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

/// macOS replaces its app bundle (asking for an administrator password if it must). On Linux only
/// an AppImage can replace itself; a .deb belongs to the package manager. `APPIMAGE` alone isn't
/// proof, as child processes inherit it; the bundler stamps the build's bundle type into the binary.
pub fn self_update_supported() -> bool {
    use tauri::utils::{config::BundleType, platform::bundle_type};
    if cfg!(target_os = "macos") {
        // An unbundled dev build would have the updater replace the folder holding the binary.
        return std::env::current_exe().is_ok_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"));
    }
    cfg!(target_os = "linux") && bundle_type() == Some(BundleType::AppImage) && running_appimage().is_some()
}

fn running_appimage() -> Option<PathBuf> {
    let var = |name| std::env::var_os(name).map(PathBuf::from);
    appimage_of(&std::env::current_exe().ok()?, &var("APPDIR")?, var("APPIMAGE")?)
}

/// The AppImage this binary runs from: only when it runs inside that AppImage's `APPDIR`, since an
/// extracted copy started by another AppImage inherits that app's variables (and would overwrite it).
fn appimage_of(exe: &Path, appdir: &Path, appimage: PathBuf) -> Option<PathBuf> {
    let (exe, appdir) = (exe.canonicalize().ok()?, appdir.canonicalize().ok()?);
    (exe.starts_with(appdir) && appimage.is_file()).then_some(appimage)
}

/// The in-app update's progress; `T` is the verified update and its bytes.
enum Stage<T> {
    Idle,
    Downloading,
    /// Verified and waiting for the user to restart.
    Ready(T),
    Installing,
}

impl<T> Stage<T> {
    /// Moves a ready update to `Installing` and hands it over; any other stage is left as it is.
    fn take_ready(&mut self) -> Option<T> {
        match std::mem::replace(self, Stage::Installing) {
            Stage::Ready(ready) => Some(ready),
            other => {
                *self = other;
                None
            }
        }
    }
}

static STAGE: Mutex<Stage<Box<(Update, Vec<u8>)>>> = Mutex::new(Stage::Idle);

const RESTART_MARKER: &str = "update-restart";

/// Left in the app data dir before restarting into an update: the restart repeats the launch's
/// arguments, which mustn't run again (`--background`, `--toggle`, …).
pub fn mark_restart(data_dir: &Path) -> std::io::Result<()> {
    std::fs::write(data_dir.join(RESTART_MARKER), "")
}

/// Whether this launch is the restart into an update. Removes the marker, so it counts once.
pub fn take_restart_marker(data_dir: &Path) -> bool {
    std::fs::remove_file(data_dir.join(RESTART_MARKER)).is_ok()
}

fn emit_download(handle: &AppHandle, state: &str, percent: Option<u64>, error: Option<String>) {
    let _ = handle.emit("update-download", json!({ "state": state, "percent": percent, "error": error }));
}

/// Downloads the latest release's update and keeps it until `install`. The outcome is also sent as
/// `update-download` events. A click while a download runs joins it; a finished one is reused.
pub async fn download(handle: &AppHandle) -> Result<()> {
    ensure!(self_update_supported(), "This copy of OpsTrellis can't update itself. Get the new version from the release page.");
    {
        let mut stage = STAGE.lock().unwrap();
        match *stage {
            Stage::Downloading | Stage::Installing => return Ok(()),
            Stage::Ready(_) => {
                emit_download(handle, "ready", Some(100), None);
                return Ok(());
            }
            Stage::Idle => *stage = Stage::Downloading,
        }
    }
    emit_download(handle, "downloading", None, None);
    let fetched = fetch(handle).await;
    let mut stage = STAGE.lock().unwrap();
    match fetched {
        Ok(ready) => {
            *stage = Stage::Ready(Box::new(ready));
            emit_download(handle, "ready", Some(100), None);
            Ok(())
        }
        Err(e) => {
            *stage = Stage::Idle;
            log::warn!("update download failed: {e:#}");
            emit_download(handle, "failed", None, Some(format!("{e:#}")));
            Err(e)
        }
    }
}

async fn fetch(handle: &AppHandle) -> Result<(Update, Vec<u8>)> {
    // A stalled connection fails instead of leaving the download running forever.
    let updater = handle.updater_builder().timeout(Duration::from_secs(20)).configure_client(|c| c.read_timeout(Duration::from_secs(30))).build()?;
    let update = updater.check().await.map_err(readable)?.ok_or_else(|| anyhow!("There's no newer version to download."))?;
    let (mut received, mut shown) = (0u64, None);
    let bytes = update
        .download(
            |chunk, total| {
                received += chunk as u64;
                let percent = total.filter(|&t| t > 0).map(|t| (received * 100 / t).min(99));
                if percent != shown {
                    shown = percent;
                    emit_download(handle, "downloading", percent, None);
                }
            },
            || {},
        )
        .await
        .map_err(readable)?;
    Ok((update, bytes))
}

fn readable(e: tauri_plugin_updater::Error) -> anyhow::Error {
    use tauri_plugin_updater::Error as E;
    let what = match &e {
        // Also what releases from before in-app updates answer: they have no latest.json.
        E::ReleaseNotFound | E::TargetNotFound(_) | E::TargetsNotFound(_) => "The latest release has no in-app update",
        E::Reqwest(_) | E::Network(_) => "Couldn't download the update from github.com",
        E::Minisign(_) | E::Base64(_) | E::SignatureUtf8(_) | E::SignedVersionMismatch { .. } | E::MissingSignedVersion => {
            "The update's signature doesn't match, so it was discarded"
        }
        _ => "The update failed",
    };
    anyhow::Error::new(e).context(what)
}

/// Installs the downloaded update over this app. The caller restarts; on an error the app is left
/// as it was and the download is kept, so the user can try again.
pub fn install(handle: &AppHandle) -> Result<()> {
    let ready = STAGE.lock().unwrap().take_ready().ok_or_else(|| anyhow!("Download the update first."))?;
    emit_download(handle, "installing", Some(100), None);
    let (update, bytes) = &*ready;
    if let Err(e) = update.install(bytes) {
        log::warn!("installing the update failed: {e}");
        let e = anyhow::Error::new(e).context("Couldn't install the update");
        *STAGE.lock().unwrap() = Stage::Ready(ready);
        emit_download(handle, "ready", Some(100), None);
        return Err(e);
    }
    *STAGE.lock().unwrap() = Stage::Idle;
    log::info!("installed OpsTrellis {}", update.version);
    if cfg!(target_os = "linux") {
        if let (Some(appimage), Some(home)) = (running_appimage(), dirs::home_dir()) {
            if let Err(e) = record_installed_version(&appimage, &home, &update.version) {
                log::warn!("couldn't update install.sh's version marker: {e:#}");
            }
        }
    }
    Ok(())
}

/// install.sh keeps `VERSION` (version, then the AppImage's sha256) next to the AppImage it manages
/// and reinstalls when they don't match. Rewrites it when `appimage` is that AppImage; returns
/// whether it did.
fn record_installed_version(appimage: &Path, home: &Path, version: &str) -> Result<bool> {
    let managed = home.join(".local/share/opstrellis/OpsTrellis.AppImage");
    let marker = managed.with_file_name("VERSION");
    let same_file = matches!((appimage.canonicalize(), managed.canonicalize()), (Ok(a), Ok(b)) if a == b);
    if !same_file || !marker.is_file() {
        return Ok(false);
    }
    let digest = crate::polish::sha256_hex(&managed)?;
    let permissions = marker.metadata()?.permissions();
    let tmp = marker.with_file_name(format!(".VERSION.{}", uuid::Uuid::new_v4()));
    let written = std::fs::write(&tmp, format!("{version}\n{digest}\n")).and_then(|_| std::fs::set_permissions(&tmp, permissions)).and_then(|_| std::fs::rename(&tmp, &marker));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.context("writing the version marker")?;
    Ok(true)
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

    fn install_sh_layout(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let home = std::env::temp_dir().join(format!("opstrellis-update-{tag}-{}", uuid::Uuid::new_v4()));
        let dir = home.join(".local/share/opstrellis");
        std::fs::create_dir_all(&dir).unwrap();
        let (image, marker) = (dir.join("OpsTrellis.AppImage"), dir.join("VERSION"));
        std::fs::write(&image, "new image").unwrap();
        (home, image, marker)
    }

    #[cfg(unix)]
    #[test]
    fn install_sh_marker_is_rewritten_in_place() {
        use std::os::unix::fs::PermissionsExt;
        let (home, image, marker) = install_sh_layout("marker");
        std::fs::write(&marker, "0.3.5\nolddigest\n").unwrap();
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();

        assert!(record_installed_version(&image, &home, "0.4.0").unwrap());
        let digest = crate::polish::sha256_hex(&image).unwrap();
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), format!("0.4.0\n{digest}\n"));
        assert_eq!(marker.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        let names: Vec<_> = std::fs::read_dir(marker.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 2, "no temporary file is left: {names:?}");

        // The same AppImage reached through a symlink (as with a symlinked home) still counts.
        let link = home.join("link.AppImage");
        std::os::unix::fs::symlink(&image, &link).unwrap();
        assert!(record_installed_version(&link, &home, "0.4.1").unwrap());
        assert!(std::fs::read_to_string(&marker).unwrap().starts_with("0.4.1\n"));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn other_appimages_leave_the_marker_alone() {
        let (home, image, marker) = install_sh_layout("other");
        let elsewhere = home.join("Downloads/OpsTrellis.AppImage");
        std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
        std::fs::write(&elsewhere, "new image").unwrap();
        std::fs::write(&marker, "0.3.5\nolddigest\n").unwrap();
        assert!(!record_installed_version(&elsewhere, &home, "0.4.0").unwrap());
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "0.3.5\nolddigest\n");

        // Not installed by install.sh: no marker is created.
        std::fs::remove_file(&marker).unwrap();
        assert!(!record_installed_version(&image, &home, "0.4.0").unwrap());
        assert!(!marker.exists());
        assert!(!record_installed_version(&home.join("missing.AppImage"), &home, "0.4.0").unwrap());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn only_a_ready_update_can_be_installed() {
        for (mut stage, kept) in [(Stage::<u8>::Idle, "idle"), (Stage::Downloading, "downloading"), (Stage::Installing, "installing")] {
            assert_eq!(stage.take_ready(), None);
            let now = match stage { Stage::Idle => "idle", Stage::Downloading => "downloading", Stage::Installing => "installing", Stage::Ready(_) => "ready" };
            assert_eq!(now, kept, "a download in progress keeps running");
        }
        let mut stage = Stage::Ready(7);
        assert_eq!(stage.take_ready(), Some(7));
        assert!(matches!(stage, Stage::Installing));
        assert_eq!(stage.take_ready(), None, "a second install gets nothing");
    }

    #[test]
    fn the_restart_marker_counts_for_one_launch() {
        let dir = std::env::temp_dir().join(format!("opstrellis-restart-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!take_restart_marker(&dir), "a normal launch");
        mark_restart(&dir).unwrap();
        assert!(take_restart_marker(&dir), "the restart into the update");
        assert!(!take_restart_marker(&dir), "the launch after it is normal again");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_the_appimage_this_binary_runs_from_counts() {
        let (home, image, _) = install_sh_layout("appdir");
        let appdir = home.join("mount");
        std::fs::create_dir_all(appdir.join("usr/bin")).unwrap();
        let exe = appdir.join("usr/bin/opstrellis");
        std::fs::write(&exe, "").unwrap();
        assert_eq!(appimage_of(&exe, &appdir, image.clone()), Some(image.clone()));
        let extracted = home.join("squashfs-root/usr/bin/opstrellis");
        std::fs::create_dir_all(extracted.parent().unwrap()).unwrap();
        std::fs::write(&extracted, "").unwrap();
        assert_eq!(appimage_of(&extracted, &appdir, image.clone()), None, "another app's APPDIR");
        assert_eq!(appimage_of(&exe, &appdir, home.join("gone.AppImage")), None);
        assert_eq!(appimage_of(&exe, &home.join("no-such-dir"), image), None);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn updater_errors_read_as_what_went_wrong() {
        use tauri_plugin_updater::Error as E;
        let text = |e| format!("{:#}", readable(e));
        assert!(text(E::ReleaseNotFound).starts_with("The latest release has no in-app update"));
        assert!(text(E::Network("Download request failed with status: 404".into())).starts_with("Couldn't download the update from github.com"));
        assert!(text(E::SignedVersionMismatch { signed: "0.3.0".into(), announced: "0.4.0".into() }).starts_with("The update's signature doesn't match"));
    }

    #[test]
    fn info_serializes_for_the_ui() {
        let v = serde_json::to_value(parse_release(FIXTURE, "0.3.2").unwrap()).unwrap();
        assert_eq!(v, serde_json::json!({ "current": "0.3.2", "latest": "v0.4.0", "newer": true, "url": "https://github.com/mtr1920/opstrellis/releases/tag/v0.4.0" }));
    }
}
