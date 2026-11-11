//! OpsTrellis Cloud: device-code sign-in, usage metadata sync and the team library sync loop
//! (`team.rs`). The API token is stored in the keychain as secret "cloud" and doubles as the key
//! for the Cloud STT/LLM providers.

use crate::stt::{check, send, HTTP};
use crate::App;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct CloudStatus {
    pub signed_in: bool,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub usage: Option<Value>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStart {
    pub user_code: String,
    pub verification_url: String,
}

static LOGIN_ATTEMPT: AtomicU64 = AtomicU64::new(0);

/// The configured server, refused when the token would travel unencrypted.
pub(crate) fn base(app: &App) -> Result<String> {
    let base = base_of(&app.settings().cloud.base_url).to_string();
    crate::endpoint::check(&base)?;
    Ok(base)
}

pub async fn login(app: Arc<App>) -> Result<LoginStart> {
    let base = base(&app)?;
    let v: Value = check("cloud", send(HTTP.post(format!("{base}/api/device/start")).json(&json!({ "device_name": device_name() }))).await?)
        .await?
        .json()
        .await?;
    let device_code = v["device_code"].as_str().ok_or_else(|| anyhow!("cloud: bad device response"))?.to_string();
    let user_code = v["user_code"].as_str().unwrap_or_default().to_string();
    let start = LoginStart {
        // Built from the configured server instead of taken from the response, so the browser only
        // ever opens that server's own sign-in page.
        verification_url: format!("{base}/device?code={}", urlencoding::encode(&user_code)),
        user_code,
    };
    let interval = v["interval"].as_u64().unwrap_or(3).max(1);
    let expires = v["expires_in"].as_u64().unwrap_or(600);
    // Never open anything but a web page (no file://, smb:// or app schemes), even if the Server URL is odd.
    if is_web_url(&start.verification_url) {
        let _ = tauri_plugin_opener::open_url(&start.verification_url, None::<&str>);
    }
    // A newer sign-in attempt supersedes this one (only one poller at a time).
    let attempt = LOGIN_ATTEMPT.fetch_add(1, Ordering::SeqCst) + 1;
    tauri::async_runtime::spawn(async move {
        let deadline = std::time::Instant::now() + Duration::from_secs(expires);
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            if LOGIN_ATTEMPT.load(Ordering::SeqCst) != attempt {
                return;
            }
            let resp = match send(HTTP.post(format!("{base}/api/device/poll")).json(&json!({ "device_code": device_code }))).await {
                Ok(r) => r,
                Err(_) => continue,
            };
            match resp.status().as_u16() {
                200 => {
                    let token = resp.json::<Value>().await.ok().and_then(|v| v["token"].as_str().map(String::from));
                    // The token belongs to the server that issued it, never to one configured since.
                    if base_of(&app.settings().cloud.base_url) != base {
                        return;
                    }
                    if let Some(token) = token {
                        if let Err(e) = app.secrets.set("cloud", &token) {
                            app.toast_error(&format!("Could not store the cloud token: {e:#}"));
                            return;
                        }
                        let st = status(&app).await;
                        let _ = app.update_settings(|s| s.cloud.email = st.email.clone());
                        let _ = app.handle.emit("cloud-status", st);
                        let _ = crate::team::sync(&app).await;
                    }
                    return;
                }
                428 => continue,
                _ => break,
            }
        }
        // Expired or denied: tell the UI so it drops the code it's showing.
        app.toast_error("Sign-in was not completed (code expired or denied).");
        let _ = app.handle.emit("cloud-status", CloudStatus::default());
    });
    Ok(start)
}

pub async fn status(app: &App) -> CloudStatus {
    let Some(token) = app.secrets.get("cloud") else { return CloudStatus::default() };
    let resp = match base(app) {
        Ok(base) => send(HTTP.get(format!("{base}/api/me")).bearer_auth(token).timeout(Duration::from_secs(10))).await,
        Err(e) => Err(e),
    };
    match resp {
        Ok(r) if r.status().is_success() => {
            let v: Value = r.json().await.unwrap_or_default();
            CloudStatus {
                signed_in: true,
                email: v.pointer("/user/email").and_then(Value::as_str).map(String::from),
                plan: v["plan"].as_str().map(String::from).or_else(|| v.pointer("/plan/id").and_then(Value::as_str).map(String::from)),
                usage: v.get("usage").cloned(),
            }
        }
        Ok(r) if r.status().as_u16() == 401 => {
            let _ = app.secrets.delete("cloud");
            let _ = crate::team::clear(app);
            CloudStatus::default()
        }
        // Offline: still signed in, details unknown.
        _ => CloudStatus { signed_in: true, email: app.settings().cloud.email, ..Default::default() },
    }
}

pub fn logout(app: &App) -> Result<()> {
    forget_account(app)?;
    app.update_settings(|s| s.cloud.email = None)?;
    Ok(())
}

/// Signed out (or moved to another server): the token, a sign-in still waiting for approval,
/// usage not yet sent and the team library all belonged to the old account. The caller clears the email.
pub fn forget_account(app: &App) -> Result<()> {
    LOGIN_ATTEMPT.fetch_add(1, Ordering::SeqCst);
    app.secrets.delete("cloud")?;
    app.db.clear_usage()?;
    crate::team::clear(app)?;
    Ok(())
}

/// The server part of a configured Server URL, as requests use it.
pub fn base_of(url: &str) -> &str {
    url.trim().trim_end_matches('/')
}

/// Sends queued usage metadata (never transcript text). Returns the number of events accepted.
pub async fn flush_usage(app: &App) -> Result<usize> {
    if !app.settings().privacy.sync_usage {
        return Ok(0);
    }
    let Some(token) = app.secrets.get("cloud") else { return Ok(0) };
    let pending = app.db.pending_usage(500)?;
    if pending.is_empty() {
        return Ok(0);
    }
    let (ids, events): (Vec<String>, Vec<Value>) = pending.into_iter().unzip();
    let resp = send(HTTP.post(format!("{}/api/usage", base(app)?)).bearer_auth(token).json(&json!({ "events": events }))).await?;
    check("cloud", resp).await?;
    app.db.ack_usage(&ids)?;
    Ok(ids.len())
}

pub fn spawn_sync_loop(app: Arc<App>) {
    tauri::async_runtime::spawn(async move {
        let (mut team_due, mut team_wait) = (std::time::Instant::now(), crate::team::INTERVAL);
        loop {
            if let Err(e) = flush_usage(&app).await {
                log::debug!("usage sync failed: {e:#}");
            }
            if std::time::Instant::now() >= team_due {
                team_wait = crate::team::next_wait(team_wait, &crate::team::sync(&app).await);
                team_due = std::time::Instant::now() + team_wait;
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
}

fn device_name() -> String {
    let host = std::env::var("HOSTNAME").ok().or_else(|| std::fs::read_to_string("/etc/hostname").ok()).unwrap_or_default();
    let os = if cfg!(target_os = "macos") { "macOS" } else { "Linux" };
    format!("OpsTrellis on {} ({os})", host.trim()).replace(" ()", "")
}

fn is_web_url(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    u.starts_with("https://") || u.starts_with("http://")
}

#[cfg(test)]
mod tests {
    #[test]
    fn opens_only_web_urls() {
        assert!(super::is_web_url("https://cloud.example.com/device?code=AB"));
        assert!(super::is_web_url("http://localhost:8787/device"));
        assert!(!super::is_web_url("file:///etc/passwd"));
        assert!(!super::is_web_url("smb://evil/share"));
        assert!(!super::is_web_url("x-custom://open"));
        assert!(!super::is_web_url(""));
    }
}
