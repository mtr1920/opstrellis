//! API keys live in the OS keychain (macOS Keychain / Linux Secret Service). When no keychain is
//! available (e.g. a headless Linux session without a Secret Service daemon) we fall back to a
//! 0600 JSON file in the app data dir and log a warning.

use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SERVICE: &str = "app.opstrellis.desktop";

pub struct Secrets {
    fallback_path: PathBuf,
    lock: Mutex<()>,
    /// False only in tests, so they never touch the real keychain.
    keychain: bool,
}

impl Secrets {
    pub fn new(data_dir: &std::path::Path) -> Self {
        Secrets { fallback_path: data_dir.join("secrets.json"), lock: Mutex::new(()), keychain: true }
    }

    #[cfg(test)]
    pub fn file_only(data_dir: &std::path::Path) -> Self {
        Secrets { keychain: false, ..Secrets::new(data_dir) }
    }

    pub fn get(&self, id: &str) -> Option<String> {
        if self.keychain {
            if let Ok(v) = keyring::Entry::new(SERVICE, id).and_then(|e| e.get_password()) {
                return Some(v);
            }
        }
        self.read_file().remove(id)
    }

    pub fn set(&self, id: &str, value: &str) -> Result<()> {
        let value = value.trim();
        let stored = if self.keychain { keyring::Entry::new(SERVICE, id).and_then(|e| e.set_password(value)) } else { Err(keyring::Error::NoEntry) };
        match stored {
            Ok(()) => {
                // Make sure no stale fallback copy remains.
                self.update_file(|m| {
                    m.remove(id);
                })
            }
            Err(e) => {
                log::warn!("keychain unavailable ({e}); storing secret in a 0600 file");
                self.update_file(|m| {
                    m.insert(id.to_string(), value.to_string());
                })
            }
        }
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        if let Some(e) = self.keychain.then(|| keyring::Entry::new(SERVICE, id).ok()).flatten() {
            let _ = e.delete_credential();
        }
        self.update_file(|m| {
            m.remove(id);
        })
    }

    pub fn has(&self, id: &str) -> bool {
        self.get(id).is_some()
    }

    fn read_file(&self) -> HashMap<String, String> {
        std::fs::read(&self.fallback_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn update_file(&self, f: impl FnOnce(&mut HashMap<String, String>)) -> Result<()> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut map = self.read_file();
        let before = map.len();
        f(&mut map);
        if map.is_empty() && before == 0 {
            return Ok(());
        }
        write_private(&self.fallback_path, &serde_json::to_vec(&map)?)
    }
}

/// Replaces `path` with `bytes`, readable only by the user (0600): written to a fresh temporary
/// file and renamed, so a crash never leaves a half-written file and an older, looser file's mode
/// doesn't carry over.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let written = opts.open(&tmp).and_then(|mut f| {
        f.write_all(bytes)?;
        f.sync_all()
    });
    let result = written.and_then(|_| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(result?)
}

/// Creates `dir` if needed and makes it private to the user (0700), also when it already existed.
/// Only creating it can fail: some filesystems refuse permission changes, and the app must still start.
pub fn private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
            log::warn!("couldn't make {} private: {e}", dir.display());
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn files_and_folders_are_private() {
        let dir = std::env::temp_dir().join(format!("opstrellis-secrets-{}", uuid::Uuid::new_v4())).join("data");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700, "an existing folder is tightened");
        private_dir(&dir.join("logs")).unwrap();
        assert_eq!(mode(&dir.join("logs")), 0o700);

        let file = dir.join("secrets.json");
        std::fs::write(&file, b"old").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&file, b"{}").unwrap();
        assert_eq!((mode(&file), std::fs::read(&file).unwrap()), (0o600, b"{}".to_vec()), "an existing file is replaced and tightened");
        assert!(!dir.join("secrets.json.tmp").exists());

        let secrets = Secrets { fallback_path: file.clone(), lock: Mutex::new(()), keychain: false };
        secrets.update_file(|m| {
            m.insert("test".into(), "value".into());
        })
        .unwrap();
        assert_eq!((mode(&file), secrets.read_file().get("test").map(String::as_str)), (0o600, Some("value")));
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }
}
