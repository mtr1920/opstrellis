use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Local,
    Openai,
    Deepgram,
    Assemblyai,
    Google,
    Azure,
    Elevenlabs,
    Cloud,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SttEntry {
    pub id: String,
    pub provider: ProviderKind,
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default = "yes")]
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Hotkeys {
    pub dictate: String,
    pub command: String,
    pub paste_last: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Llm {
    pub enabled: bool,
    /// "openai" (any OpenAI-compatible endpoint), "anthropic" or "cloud".
    pub provider: String,
    pub base_url: Option<String>,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Cleanup {
    pub remove_fillers: bool,
    pub voice_commands: bool,
    /// Learn dictionary entries from corrections the user makes to dictations in History.
    pub auto_learn: bool,
    /// Fix mis-hearings of built-in tech and workplace terms, and hint the ones the user says.
    pub tech_terms: bool,
    /// Ids of the other term packs in use (built-in or imported; `terms.rs`).
    pub term_packs: Vec<String>,
}

impl Cleanup {
    /// Whether any term pack is in use.
    pub fn terms_on(&self) -> bool {
        self.tech_terms || !self.term_packs.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Insertion {
    /// "paste" or "type"
    pub method: String,
    pub restore_clipboard: bool,
    pub paste_delay_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Privacy {
    /// None = keep forever, Some(0) = don't save history.
    pub history_retention_days: Option<u32>,
    pub sync_usage: bool,
    /// Recordings kept for a retry: "off" (failed ones in memory only), "failed" or "all" (also on
    /// disk for 24 hours, while history is on). See `recovery.rs`.
    pub audio_retention: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Cloud {
    pub base_url: String,
    pub email: Option<String>,
    /// Sync the team library down while signed in (`team.rs`).
    pub team_library: bool,
}

/// What AI agents may use through `opstrellis mcp` and the CLI (`mcp.rs`, `agent.rs`). The user's
/// consent, not a security boundary: software running as the user can read the database anyway.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Agents {
    pub enabled: bool,
    /// Dictation history and the dictionary.
    pub history: bool,
    /// Notes and meeting notes.
    pub notes: bool,
    /// Adding vocabulary hints (never replacements) to the dictionary.
    pub add_terms: bool,
    /// Asking the user a question in the pill, answered by dictating (`ask.rs`).
    pub voice: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub hotkeys: Hotkeys,
    pub microphone: Option<String>,
    pub language: String,
    #[serde(deserialize_with = "lenient_chain")]
    pub stt_chain: Vec<SttEntry>,
    pub llm: Llm,
    pub cleanup: Cleanup,
    /// category ("personal" | "work" | "email" | "ai" | "other") -> tone
    pub styles: HashMap<String, String>,
    /// category -> transform id applied automatically after every dictation (AI required).
    pub auto_transforms: HashMap<String, i64>,
    /// The user's app rules: lowercase app name or window-title text -> insight category
    /// (`context::CATEGORIES`). They win over the built-in rules.
    pub app_categories: HashMap<String, String>,
    /// category -> how the user writes there ("sign off with 'Cheers, M'"), for AI cleanup.
    pub style_notes: HashMap<String, String>,
    /// AI prompts: "clean" (tidy, keep wording) or "structured" (long prompts get light sections).
    pub prompt_style: String,
    /// category -> language code (`llm::OUTPUT_LANGUAGES`) AI polish writes in; none = as spoken.
    pub output_languages: HashMap<String, String>,
    /// Command mode: show a rewrite before it replaces the selection: "long" | "always" | "never".
    pub review_rewrites: String,
    pub insertion: Insertion,
    pub privacy: Privacy,
    pub cloud: Cloud,
    pub overlay: bool,
    pub sounds: bool,
    pub launch_at_login: bool,
    /// macOS: off keeps OpsTrellis out of the Dock and ⌘Tab; it lives in the menu bar.
    pub show_in_dock: bool,
    /// "system" | "light" | "dark"
    pub theme: String,
    pub onboarded: bool,
    /// Once a day, ask GitHub for the latest version number. Off until the user turns it on.
    pub check_updates: bool,
    /// Unix seconds of the last successful automatic update check. Owned by the backend: a saved UI copy never replaces it.
    pub update_checked_at: Option<i64>,
    pub agents: Agents,
    /// Folders indexed for repo-aware dictation (`workspace.rs`), canonical absolute paths, at most
    /// 10. Owned by the backend (add_workspace / remove_workspace): a saved UI copy never replaces it.
    pub workspaces: Vec<String>,
    /// Write a workspace's names said as words as code without AI, in editors and terminals. Off
    /// by default: it can't tell a name from an ordinary phrase ("plain text" -> plain_text).
    pub workspace_replace: bool,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Settings::default().hotkeys
    }
}

impl Default for Llm {
    fn default() -> Self {
        Settings::default().llm
    }
}

impl Default for Cleanup {
    fn default() -> Self {
        Settings::default().cleanup
    }
}

impl Default for Insertion {
    fn default() -> Self {
        Settings::default().insertion
    }
}

impl Default for Privacy {
    fn default() -> Self {
        Settings::default().privacy
    }
}

impl Default for Cloud {
    fn default() -> Self {
        Settings::default().cloud
    }
}

impl Default for Agents {
    fn default() -> Self {
        Settings::default().agents
    }
}

/// Drops provider entries that no longer parse (e.g. a provider removed in a later version)
/// instead of failing — and resetting — the whole settings file.
fn lenient_chain<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<SttEntry>, D::Error> {
    let raw: Vec<serde_json::Value> = Deserialize::deserialize(d)?;
    Ok(raw.into_iter().filter_map(|v| serde_json::from_value(v).map_err(|e| log::warn!("skipping provider entry: {e}")).ok()).collect())
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            hotkeys: Hotkeys {
                dictate: "Ctrl+Shift+Space".into(),
                command: "Ctrl+Alt+Space".into(),
                paste_last: "Ctrl+Alt+V".into(),
            },
            microphone: None,
            language: "auto".into(),
            stt_chain: vec![SttEntry {
                id: "local-default".into(),
                provider: ProviderKind::Local,
                model: "moonshine-tiny-en".into(),
                base_url: None,
                region: None,
                streaming: false,
                enabled: true,
            }],
            llm: Llm {
                enabled: false,
                provider: "openai".into(),
                base_url: None,
                model: "gpt-4o-mini".into(),
            },
            cleanup: Cleanup { remove_fillers: true, voice_commands: true, auto_learn: false, tech_terms: true, term_packs: Vec::new() },
            auto_transforms: HashMap::new(),
            app_categories: HashMap::new(),
            style_notes: HashMap::new(),
            prompt_style: "clean".into(),
            output_languages: HashMap::new(),
            review_rewrites: "long".into(),
            styles: [("personal", "casual"), ("work", "casual"), ("email", "formal"), ("ai", "formal"), ("other", "formal")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            insertion: Insertion { method: "paste".into(), restore_clipboard: true, paste_delay_ms: 250 },
            privacy: Privacy { history_retention_days: None, sync_usage: true, audio_retention: "off".into() },
            cloud: Cloud { base_url: "http://localhost:8787".into(), email: None, team_library: true },
            overlay: true,
            sounds: true,
            launch_at_login: false,
            show_in_dock: true,
            theme: "system".into(),
            onboarded: false,
            check_updates: false,
            update_checked_at: None,
            workspaces: Vec::new(),
            workspace_replace: false,
            agents: Agents { enabled: false, history: true, notes: true, add_terms: false, voice: false },
        }
    }
}

impl Settings {
    pub fn load(path: &PathBuf) -> Settings {
        let Ok(text) = std::fs::read_to_string(path) else { return Settings::default() };
        let mut s: Settings = serde_json::from_str(&text).unwrap_or_else(|e| {
            // Keep the user's file: the next save would otherwise overwrite it with defaults.
            log::warn!("settings parse error ({e}); backed up to settings.json.bak");
            let _ = crate::secrets::write_private(&path.with_extension("json.bak"), text.as_bytes());
            Settings::default()
        });
        s.migrate();
        s
    }

    /// v0.4: AI chats got their own style category; it starts as "other" was, which used to cover
    /// them. Runs once: afterwards `styles` has an "ai" entry.
    fn migrate(&mut self) {
        if !self.styles.contains_key("ai") {
            let tone = self.tone("other").to_string();
            self.styles.insert("ai".into(), tone);
            if let Some(id) = self.auto_transforms.get("other").copied() {
                self.auto_transforms.insert("ai".into(), id);
            }
        }
    }

    /// Atomic and 0600: it holds the cloud email and server, and provider endpoints.
    pub fn save(&self, path: &PathBuf) -> anyhow::Result<()> {
        crate::secrets::write_private(path, &serde_json::to_vec_pretty(self)?)
    }

    pub fn tone(&self, category: &str) -> &str {
        self.styles.get(category).map(String::as_str).unwrap_or("formal")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_and_future_settings_keep_user_values() {
        let json = r#"{
            "language": "de",
            "llm": { "enabled": true, "provider": "anthropic", "model": "claude-haiku-4-5" },
            "insertion": { "method": "type" },
            "sttChain": [
                { "id": "a", "provider": "someday-provider", "model": "x" },
                { "id": "b", "provider": "deepgram", "model": "nova-3" }
            ]
        }"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.language, "de");
        assert!(s.llm.enabled);
        assert_eq!(s.llm.base_url, None);
        assert_eq!(s.insertion.method, "type");
        assert_eq!(s.insertion.paste_delay_ms, Settings::default().insertion.paste_delay_ms);
        assert_eq!(s.stt_chain.len(), 1);
        assert_eq!(s.stt_chain[0].provider, ProviderKind::Deepgram);
    }

    #[test]
    fn ai_style_starts_like_other_once() {
        let dir = std::env::temp_dir().join(format!("opstrellis-settings-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{ "styles": { "other": "casual" }, "autoTransforms": { "other": 3 } }"#).unwrap();
        let mut s = Settings::load(&path);
        assert_eq!((s.tone("ai"), s.auto_transforms.get("ai")), ("casual", Some(&3)));
        // Turning the AI action off sticks across restarts.
        s.auto_transforms.remove("ai");
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).auto_transforms.get("ai"), None);
        assert_eq!(Settings::default().tone("ai"), "formal");
    }

    #[test]
    fn unreadable_file_is_backed_up() {
        let dir = std::env::temp_dir().join(format!("opstrellis-settings-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(Settings::load(&path).language, "auto");
        assert_eq!(std::fs::read_to_string(dir.join("settings.json.bak")).unwrap(), "{ not json");
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("opstrellis-settings-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        Settings::default().save(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(Settings::load(&path).language, "auto");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
