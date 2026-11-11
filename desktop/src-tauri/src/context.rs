//! What the user is dictating into: focused app name and window title, mapped to an insight
//! category, a style category and whether it is a terminal (which needs a different paste shortcut).

use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct AppContext {
    pub app_name: Option<String>,
    pub title: Option<String>,
    /// Insight category: ai_prompts | work_messages | emails | documents | personal_messages | other
    pub category: &'static str,
    pub is_terminal: bool,
    /// A terminal running an AI coding agent (Claude Code, Codex, Aider…): dictation is a prompt,
    /// not a shell command.
    pub is_agent: bool,
    /// The focused window at key-down (process id, window id), when the platform reports it.
    pub window: Option<(u64, String)>,
}

impl AppContext {
    /// Terminals and code editors: text should stay verbatim (no tone/capitalization).
    pub fn is_technical(&self) -> bool {
        let app = self.app_name.as_deref().unwrap_or("").to_lowercase();
        self.is_terminal
            || ["code", "cursor", "zed", "intellij", "pycharm", "webstorm", "goland", "rustrover", "vim", "emacs", "sublime", "xcode"]
                .iter()
                .any(|e| app.contains(e))
    }

    /// Style category used for tone: personal | work | email | ai | other
    pub fn style_category(&self) -> &'static str {
        match self.category {
            "personal_messages" => "personal",
            "work_messages" => "work",
            "emails" => "email",
            "ai_prompts" => "ai",
            _ => "other",
        }
    }

    /// Text may be rewritten (auto actions): everywhere except shells.
    pub fn allows_rewrite(&self) -> bool {
        !self.is_terminal || self.is_agent
    }
}

/// The focused app, classified with the user's own rules (`overrides`: needle → insight category).
pub fn current(overrides: &HashMap<String, String>) -> AppContext {
    match active_win_pos_rs::get_active_window() {
        Ok(w) => AppContext { window: Some((w.process_id, w.window_id)), ..classify_with(Some(w.app_name), Some(w.title), overrides) },
        Err(_) => classify_with(None, None, overrides),
    }
}

/// The app now focused, when it is no longer the window the dictation started in (the user
/// switched windows while it was processing). None when it's the same window, or unknown.
pub fn focus_moved(ctx: &AppContext) -> Option<String> {
    let (pid, id) = ctx.window.as_ref()?;
    // Known at key-down but unreadable now (the desktop, a window-less app): not the same window.
    let Ok(now) = active_win_pos_rs::get_active_window() else { return Some("another window".into()) };
    moved(pid, id, now.process_id, &now.window_id, &now.title).then_some(now.app_name)
}

fn moved(pid: &u64, id: &str, now_pid: u64, now_id: &str, now_title: &str) -> bool {
    // Our own overlay pill can take focus on some desktops; that isn't the user moving away.
    let overlay = now_pid == u64::from(std::process::id()) && now_title == "OpsTrellis overlay";
    !overlay && (now_pid, now_id) != (*pid, id)
}

/// Insight categories a user rule may assign.
pub const CATEGORIES: &[&str] = &["ai_prompts", "work_messages", "emails", "documents", "personal_messages", "other"];

/// Window titles of AI coding agents running in a terminal. Matched in the title only, so a shell
/// whose app name happens to contain one of them isn't mistaken for an agent.
const AGENT_TITLES: &[&str] = &["claude code", "\u{2733}", "codex", "aider", "gemini", "opencode", "cursor-agent", "copilot"];

const TERMINALS: &[&str] = &[
    "terminal", "konsole", "alacritty", "kitty", "wezterm", "xterm", "urxvt", "rxvt", "tilix", "terminator",
    "foot", "ghostty", "warp", "iterm", "hyper", "st-256color", "guake", "yakuake", "ptyxis", "blackbox",
];
// (needle, category) — matched against "app name + window title", lowercase. First match wins, so
// specific web apps in browser titles come before generic ones.
const RULES: &[(&str, &str)] = &[
    ("chatgpt", "ai_prompts"),
    ("claude", "ai_prompts"),
    ("gemini", "ai_prompts"),
    ("perplexity", "ai_prompts"),
    ("copilot", "ai_prompts"),
    ("cursor", "ai_prompts"),
    ("windsurf", "ai_prompts"),
    ("gmail", "emails"),
    ("outlook", "emails"),
    ("thunderbird", "emails"),
    ("superhuman", "emails"),
    ("proton mail", "emails"),
    ("spark", "emails"),
    ("mail", "emails"),
    ("slack", "work_messages"),
    ("microsoft teams", "work_messages"),
    ("teams", "work_messages"),
    ("mattermost", "work_messages"),
    ("zulip", "work_messages"),
    ("google chat", "work_messages"),
    ("discord", "personal_messages"),
    ("whatsapp", "personal_messages"),
    ("telegram", "personal_messages"),
    ("signal", "personal_messages"),
    ("messenger", "personal_messages"),
    ("messages", "personal_messages"),
    ("imessage", "personal_messages"),
    ("google docs", "documents"),
    ("docs.google", "documents"),
    ("word", "documents"),
    ("pages", "documents"),
    ("notion", "documents"),
    ("obsidian", "documents"),
    ("libreoffice", "documents"),
    ("confluence", "documents"),
    ("notes", "documents"),
];

pub fn classify(app_name: Option<String>, title: Option<String>) -> AppContext {
    classify_with(app_name, title, &HashMap::new())
}

pub fn classify_with(app_name: Option<String>, title: Option<String>, overrides: &HashMap<String, String>) -> AppContext {
    let app = app_name.clone().unwrap_or_default().to_lowercase();
    let low_title = title.clone().unwrap_or_default().to_lowercase();
    let hay = format!("{app} {low_title}");
    let is_terminal = TERMINALS.iter().any(|t| app.contains(t));
    let is_agent = is_terminal && AGENT_TITLES.iter().any(|t| contains_agent(&low_title, t));
    // The user's rules win; the longest matching needle is the most specific.
    let user = user_category(&hay, overrides);
    let category = user
        .or(is_agent.then_some("ai_prompts"))
        .or_else(|| RULES.iter().find(|(needle, _)| contains_word(&hay, needle)).map(|(_, c)| *c))
        .unwrap_or("other");
    AppContext {
        app_name: app_name.filter(|s| !s.is_empty()),
        title: title.filter(|s| !s.is_empty()),
        category,
        is_terminal,
        is_agent: is_terminal && user.map_or(is_agent, |c| c == "ai_prompts"),
        window: None,
    }
}

/// History does not store window titles. Keep its captured category (e.g. Gmail in a browser),
/// unless a current user rule for the app explicitly changes it.
pub fn from_history(app_name: Option<String>, category: &str, overrides: &HashMap<String, String>) -> AppContext {
    let mut ctx = classify_with(app_name, None, overrides);
    if user_category(&ctx.app_name.as_deref().unwrap_or("").to_lowercase(), overrides).is_none() {
        if let Some(category) = CATEGORIES.iter().find(|c| **c == category) {
            ctx.category = category;
            ctx.is_agent = ctx.is_terminal && *category == "ai_prompts";
        }
    }
    ctx
}

fn user_category(hay: &str, overrides: &HashMap<String, String>) -> Option<&'static str> {
    overrides.iter()
        .filter(|(needle, c)| !needle.trim().is_empty() && CATEGORIES.contains(&c.as_str()) && hay.contains(&needle.trim().to_lowercase()))
        .max_by_key(|(needle, _)| (needle.trim().len(), needle.as_str()))
        .and_then(|(_, c)| CATEGORIES.iter().find(|k| **k == c.as_str()).copied())
}

/// `needle` occurs in `hay` as a whole word ("word" in "Microsoft Word", not in "password").
pub(crate) fn contains_word(hay: &str, needle: &str) -> bool {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric());
    hay.match_indices(needle).any(|(i, m)| !word(hay[..i].chars().next_back()) && !word(hay[i + m.len()..].chars().next()))
}

/// A directory named after an agent is common in shell titles; it does not permit rewrites.
fn contains_agent(title: &str, needle: &str) -> bool {
    let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && !matches!(c, '/' | '\\' | '_'));
    title.match_indices(needle).any(|(i, m)| {
        boundary(title[..i].chars().next_back()) && boundary(title[i + m.len()..].chars().next())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_apps() {
        let c = classify(Some("Google Chrome".into()), Some("Inbox (3) - me@x.com - Gmail".into()));
        assert_eq!((c.category, c.style_category()), ("emails", "email"));
        assert_eq!(classify(Some("Slack".into()), None).style_category(), "work");
        assert_eq!(classify(Some("WhatsApp".into()), None).style_category(), "personal");
        assert_eq!(classify(Some("firefox".into()), Some("ChatGPT".into())).category, "ai_prompts");
        let t = classify(Some("gnome-terminal-server".into()), Some("~/code".into()));
        assert!(t.is_terminal);
        assert_eq!(t.style_category(), "other");
        assert!(!t.allows_rewrite());
        // Whole words only.
        assert_eq!(classify(Some("KeePassXC".into()), Some("Password database".into())).category, "other");
        assert_eq!(classify(Some("firefox".into()), Some("WordPress admin".into())).category, "other");
        assert_eq!(classify(Some("LibreOffice Writer".into()), Some("report.odt".into())).category, "documents");
        assert_eq!(classify(Some("ChatGPT".into()), None).style_category(), "ai");
    }

    #[test]
    fn agents_in_terminals_are_prompts() {
        let a = classify(Some("kitty".into()), Some("\u{2733} Fix the flaky test".into()));
        assert!(a.is_agent && a.allows_rewrite());
        assert_eq!(a.category, "ai_prompts");
        let a = classify(Some("gnome-terminal-server".into()), Some("codex — ~/src/app".into()));
        assert!(a.is_agent);
        // A plain shell, even in a folder named after a tool, stays a shell.
        let s = classify(Some("kitty".into()), Some("~/src/codexify".into()));
        assert!(!s.is_agent && !s.allows_rewrite());
        // Not a terminal: no agent flag (the browser rules decide).
        assert!(!classify(Some("firefox".into()), Some("Gemini".into())).is_agent);
    }

    #[test]
    fn focus_moved_only_to_another_window() {
        let me = u64::from(std::process::id());
        assert!(!moved(&42, "0x1", 42, "0x1", "Inbox"), "same window, new title (another tab or channel)");
        assert!(moved(&42, "0x1", 42, "0x2", "Other"), "another window of the same app");
        assert!(moved(&42, "0x1", 7, "0x9", "Terminal"), "another app");
        assert!(!moved(&42, "0x1", me, "0x5", "OpsTrellis overlay"), "our pill isn't a move");
        assert!(moved(&42, "0x1", me, "0x6", "OpsTrellis"), "our main window is");
        assert_eq!(focus_moved(&classify(Some("Slack".into()), None)), None, "unknown at key-down: never blocks the paste");
    }

    #[test]
    fn user_rules_win() {
        let rules: HashMap<String, String> =
            [("thunderbird".to_string(), "work_messages".to_string()), ("my-crm".into(), "emails".into()), ("bad".into(), "nope".into())].into();
        assert_eq!(classify_with(Some("Thunderbird".into()), None, &rules).category, "work_messages");
        assert_eq!(classify_with(Some("firefox".into()), Some("Leads - My-CRM".into()), &rules).category, "emails");
        // Unknown categories are ignored.
        assert_eq!(classify_with(Some("bad app".into()), None, &rules).category, "other");
        // A terminal the user marks as an AI prompt is treated as an agent.
        let r: HashMap<String, String> = [("wezterm".to_string(), "ai_prompts".to_string())].into();
        assert!(classify_with(Some("wezterm".into()), Some("zsh".into()), &r).allows_rewrite());
    }

    #[test]
    fn shells_named_after_agents_remain_protected() {
        for title in ["me@host: ~/src/codex", "/work/aider/src", "C:\\src\\gemini", "~/claude code"] {
            let ctx = classify(Some("kitty".into()), Some(title.into()));
            assert!(!ctx.is_agent && !ctx.allows_rewrite(), "{title}");
        }
        let overrides = [("kitty".into(), "other".into())].into();
        let ctx = classify_with(Some("kitty".into()), Some("Codex — project".into()), &overrides);
        assert_eq!(ctx.category, "other");
        assert!(!ctx.is_agent && !ctx.allows_rewrite(), "explicit shell correction wins");
    }

    #[test]
    fn history_keeps_captured_category_and_respects_current_rules() {
        let ctx = from_history(Some("Firefox".into()), "emails", &HashMap::new());
        assert_eq!(ctx.style_category(), "email");
        let ctx = from_history(Some("kitty".into()), "ai_prompts", &HashMap::new());
        assert!(ctx.is_agent && ctx.allows_rewrite());
        let rules = [("firefox".into(), "work_messages".into()), ("firefox invalid".into(), "bad".into())].into();
        assert_eq!(from_history(Some("Firefox".into()), "emails", &rules).style_category(), "work");
        assert_eq!(classify_with(Some("Firefox invalid".into()), None, &rules).category, "work_messages");
    }
}
