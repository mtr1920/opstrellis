use anyhow::Result;
use chrono::{Duration, Local, NaiveDate};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

pub struct Db(Mutex<Connection>);

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItem {
    pub id: i64,
    pub created_at: String,
    pub raw_text: String,
    pub text: String,
    pub app_name: Option<String>,
    pub category: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub audio_ms: i64,
    pub latency_ms: i64,
    pub words: i64,
    pub status: String,
    pub error: Option<String>,
    pub mode: String,
    #[serde(skip)]
    pub fixes: i64,
    /// Not stored: whether a recording of it can be transcribed again (`recovery.rs`).
    pub has_audio: bool,
}

/// Result of learning from a correction: entries added (or changed), corrections that will be
/// learned if the user makes them again, and how to undo the changes.
#[derive(Debug, Default, Serialize)]
pub struct Learned {
    pub added: Vec<DictEntry>,
    pub pending: Vec<crate::learn::Correction>,
    pub undo: Vec<UndoRow>,
    /// A fix was not learned because the dictionary is full (`DICT_MAX_ENTRIES`, `DICT_MAX_BYTES`).
    pub full: bool,
}

/// A dictionary row's (phrase, replacement), or None when there is no row.
type RowState = Option<(String, Option<String>)>;

/// One dictionary row that learning changed, before and after.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoRow {
    pub id: i64,
    pub before: RowState,
    pub after: RowState,
}

/// Runs a statement that changes dictionary row `id`, recording the row before and after.
fn tracked(c: &Connection, undo: &mut Vec<UndoRow>, id: i64, sql: &str, p: impl rusqlite::Params) -> Result<()> {
    let before = row_state(c, id)?;
    c.execute(sql, p)?;
    undo.push(UndoRow { id, before, after: row_state(c, id)? });
    Ok(())
}

fn row_state(c: &Connection, id: i64) -> Result<RowState> {
    Ok(c.query_row("SELECT phrase, replacement FROM dictionary WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictEntry {
    #[serde(default)]
    pub id: Option<i64>,
    pub phrase: String,
    pub replacement: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    /// Added automatically from a correction (auto-learn), not typed in by the user.
    #[serde(default)]
    pub learned: bool,
}

/// A dictionary entry and where it came from: "manual", "learned", "agent" (`opstrellis mcp`) or "team".
#[derive(Debug, Clone, Serialize)]
pub struct DictRow {
    #[serde(flatten)]
    pub entry: DictEntry,
    pub source: String,
    /// Why a team entry is not used (see `team_unused`); None when it is, and for the user's own.
    pub unused: Option<&'static str>,
}

/// A snippet or action as the library pages list it: whether it came from the team library, and
/// why a team entry is not used.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryRow<T> {
    #[serde(flatten)]
    pub entry: T,
    pub team: bool,
    pub unused: Option<&'static str>,
}

/// Team entries past these are not used (the server allows 2000 library entries in all). Every
/// snippet trigger is compiled and matched on every dictation, about 0.2 ms each (`text.rs` snippet_speed).
pub const TEAM_MAX_SNIPPETS: usize = 50;
pub const TEAM_MAX_ACTIONS: usize = 200;
pub const TEAM_READ_ONLY: &str = "Team entries are edited in the team dashboard";

/// Why each team row is not used: "shadowed" when its key is one of the keys of the user's own
/// rows (or a built-in), else "limit" from the first that `fits` refuses, in the order given (oldest
/// first). None for the user's own rows and team rows in use.
fn team_unused<T>(
    rows: &[T],
    team: impl Fn(&T) -> bool,
    own_keys: impl Fn(&T) -> Vec<String>,
    team_key: impl Fn(&T) -> String,
    mut fits: impl FnMut(&T) -> bool,
) -> Vec<Option<&'static str>> {
    let own: std::collections::HashSet<String> = rows.iter().filter(|r| !team(r)).flat_map(&own_keys).collect();
    let mut full = false;
    rows.iter()
        .map(|r| {
            if !team(r) {
                None
            } else if own.contains(&team_key(r)) {
                Some("shadowed")
            } else if full || !fits(r) {
                full = true;
                Some("limit")
            } else {
                None
            }
        })
        .collect()
}

/// Whether `entries` build into the dictionary matcher, remembered for the last dictionary checked
/// (it changes only when an entry does).
fn matcher_builds(entries: &[DictEntry]) -> bool {
    use std::hash::{Hash, Hasher};
    static LAST: Mutex<Option<(u64, bool)>> = Mutex::new(None);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for e in entries {
        (&e.phrase, &e.replacement).hash(&mut h);
    }
    let key = h.finish();
    if let Some((k, ok)) = *LAST.lock().unwrap_or_else(|e| e.into_inner()) {
        if k == key {
            return ok;
        }
    }
    let ok = crate::text::dictionary_builds(entries);
    *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, ok));
    ok
}

/// A snippet trigger as the matchers compare it (`text::normalize` also drops end punctuation).
fn trigger_key(trigger: &str) -> String {
    crate::terms::norm(trigger.trim().trim_end_matches(['.', '!', '?', ',']))
}

/// Refuses to change row `id` of `table` when it came from the team library.
fn ensure_own(c: &Connection, table: &str, id: Option<i64>) -> Result<()> {
    let Some(id) = id else { return Ok(()) };
    // The dictionary marks team rows by source too, which databases opened without migrations (`open_existing`) have.
    let team = if table == "dictionary" { "source = 'team'" } else { "team_id IS NOT NULL" };
    let is_team: bool = c.query_row(&format!("SELECT {team} FROM {table} WHERE id = ?1"), [id], |r| r.get(0)).optional()?.unwrap_or(false);
    anyhow::ensure!(!is_team, TEAM_READ_ONLY);
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    #[serde(default)]
    pub id: Option<i64>,
    pub trigger: String,
    pub expansion: String,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transform {
    #[serde(default)]
    pub id: Option<i64>,
    pub name: String,
    pub prompt: String,
    #[serde(default)]
    pub builtin: bool,
    /// Command mode: "replace" the selection (or insert at the cursor), or "copy" the result.
    #[serde(default = "replace")]
    pub output: String,
}

fn replace() -> String {
    "replace".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    #[serde(default)]
    pub id: Option<i64>,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub updated_at: Option<String>,
    /// "note" (Scratchpad) or "meeting" (Notetaker).
    #[serde(default = "note_kind")]
    pub kind: String,
}

fn note_kind() -> String {
    "note".into()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUse {
    pub app_name: String,
    pub category: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayWords {
    pub date: String,
    pub words: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryStat {
    pub category: String,
    pub count: i64,
    pub words: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub total_words: i64,
    pub total_dictations: i64,
    pub wpm: i64,
    pub fixes: i64,
    pub streak: i64,
    pub longest_streak: i64,
    pub days: Vec<DayWords>,
    pub categories: Vec<CategoryStat>,
}

/// Usage metadata queued for OpsTrellis Cloud. Never contains transcript text.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct UsageEvent {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub mode: String,
    pub audio_seconds: f64,
    pub words: i64,
    pub app_category: String,
    pub created_at: String,
}

/// The matcher compiles the whole dictionary into one regex; past roughly this size it fails to
/// build and no entry applies any more.
pub const DICT_MAX_ENTRIES: usize = 2000;
pub const DICT_MAX_BYTES: usize = 150_000;

/// Words, triggers and names are matched and typed into other apps: a stray `\r` would press Enter.
pub fn plain_line(s: &str) -> bool {
    !s.chars().any(char::is_control)
}

/// Multi-line text (expansions, prompts) may hold newlines and tabs, no other control characters.
pub fn plain_text(s: &str) -> bool {
    s.chars().all(|c| !c.is_control() || c == '\n' || c == '\t')
}

/// Entries and bytes (phrases plus replacements) of the user's own dictionary, leaving out row
/// `except`. Team entries only fill what is left (`list_dictionary_rows`).
pub fn dictionary_usage(c: &Connection, except: Option<i64>) -> Result<(usize, usize)> {
    let (n, bytes): (i64, i64) = c.query_row(
        "SELECT COUNT(*), COALESCE(SUM(LENGTH(CAST(phrase AS BLOB)) + LENGTH(CAST(COALESCE(replacement, '') AS BLOB))), 0) FROM dictionary WHERE id IS NOT ?1 AND source != 'team'",
        [except],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok((n as usize, bytes as usize))
}

const BUILTIN_TRANSFORMS: &[(&str, &str)] = &[
    ("Polish", "Fix grammar, spelling and awkward phrasing so the text reads clearly. Keep the author's meaning, tone and every detail; add nothing and drop nothing. If already correct, return it unchanged."),
    ("Prompt engineer", "Rewrite text as clear prompt for AI assistant, in author's own voice. Fix grammar, spelling, awkward phrasing. Main request first, then author's details and limits; separate requests as \"- \" bullets. Code, commands, file names in backticks; exact labels and messages in double quotes. Same scope: request about one part stays about that part. Add nothing author did not say: no requirements, steps, assumptions, output format. Output the prompt only: never answer it, refuse, or ask for missing things. Already clear: return unchanged."),
    ("Make formal", "Rewrite the text in a formal, professional tone. Change only the tone and word choice: keep the meaning and every detail, and add nothing."),
    ("Shorten", "Shorten the text to about half its length by cutting filler, repetition and minor asides. Keep every key point, name, number and date, and the author's tone; add nothing."),
    ("Bullet points", "Turn the text into a list of \"- \" bullets: one short bullet per point, in the original order. Keep every point and detail; add nothing."),
];
/// Added in v0.2; their results go to the clipboard (the selection is a message to answer or a
/// text to digest, not text to replace). Seeded once, also into existing databases.
const BUILTIN_COPY_ACTIONS: &[(&str, &str)] = &[
    ("Reply", "Write a reply to the selected message, from the user's point of view, saying what the user asked to say. If the user gave no direction, write a brief, neutral reply. Do not invent facts, promises or dates. Match the message's language and formality. Output only the reply text, with no subject line or placeholders."),
    ("Summarize", "Summarize the selected text in a few short \"- \" bullet points: the key facts, decisions, requests and deadlines. Use only what the text says; add no opinions or advice."),
];
/// Whether `prompt` is one of today's built-in action prompts, i.e. the user has not edited it.
pub fn is_builtin_prompt(prompt: &str) -> bool {
    BUILTIN_TRANSFORMS.iter().chain(BUILTIN_COPY_ACTIONS).any(|(_, p)| *p == prompt)
}

/// Built-in prompts of earlier versions, by action: a row still holding one gets today's
/// prompt, a prompt the user edited stays theirs.
const RETIRED_PROMPTS: &[(&str, &str)] = &[
    ("Polish", "Rewrite the text to be clear, concise and well structured. Fix grammar and flow. Keep the author's meaning, voice and language."),
    ("Prompt engineer", "Rewrite the text as a clear, well-structured prompt for an AI assistant: state the goal, context, constraints and the expected output format."),
    ("Make formal", "Rewrite the text in a professional, formal tone."),
    ("Shorten", "Make the text about half as long while keeping every key point."),
    ("Bullet points", "Turn the text into a concise bulleted list."),
    ("Reply", "Write a reply to the selected message, from the user's point of view, following what the user said they want to answer. Match the message's language and level of formality. Output only the reply text, without a subject line or placeholders."),
    ("Summarize", "Summarize the selected text in a few short bullet points: the key facts, decisions and any requests or deadlines."),
    ("Prompt engineer", "Rewrite the text as a clear prompt for an AI assistant, written as the author's own request. Put the main request first, then the details and limits the author gave; list separate requests as short bullets. Keep the scope exactly as the author set it: what they said about one part stays about that part only. Add no requirements, steps, assumptions or output format they did not state, and do not answer the prompt."),
    ("Bullet points", "Turn the text into a bulleted list: one short bullet per point, in the original order. Keep every point and detail; add nothing."),
    ("Summarize", "Summarize the selected text in a few short bullet points: the key facts, decisions, requests and deadlines. Use only what the text says; add no opinions or advice."),
];

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // Deleted dictations are overwritten, not just unlinked from the b-tree.
        conn.pragma_update(None, "secure_delete", "ON")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS history (
                id INTEGER PRIMARY KEY, created_at TEXT NOT NULL, raw_text TEXT NOT NULL, text TEXT NOT NULL,
                app_name TEXT, category TEXT NOT NULL, provider TEXT, model TEXT, audio_ms INTEGER NOT NULL,
                latency_ms INTEGER NOT NULL, words INTEGER NOT NULL, status TEXT NOT NULL, error TEXT,
                mode TEXT NOT NULL, fixes INTEGER NOT NULL DEFAULT 0);
             CREATE INDEX IF NOT EXISTS history_created ON history(created_at);
             -- Aggregates survive history deletion/retention so insights stay accurate.
             CREATE TABLE IF NOT EXISTS daily_stats (
                date TEXT NOT NULL, category TEXT NOT NULL, dictations INTEGER NOT NULL, words INTEGER NOT NULL,
                audio_ms INTEGER NOT NULL, fixes INTEGER NOT NULL, PRIMARY KEY (date, category));
             CREATE TABLE IF NOT EXISTS dictionary (id INTEGER PRIMARY KEY, phrase TEXT NOT NULL, replacement TEXT, created_at TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS snippets (id INTEGER PRIMARY KEY, trigger TEXT NOT NULL, expansion TEXT NOT NULL, created_at TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS transforms (id INTEGER PRIMARY KEY, name TEXT NOT NULL, prompt TEXT NOT NULL, builtin INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, title TEXT NOT NULL, body TEXT NOT NULL, updated_at TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS usage_outbox (id TEXT PRIMARY KEY, json TEXT NOT NULL);",
        )?;
        // v0.2: learned dictionary entries.
        let has_source: i64 = conn.query_row("SELECT COUNT(*) FROM pragma_table_info('dictionary') WHERE name = 'source'", [], |r| r.get(0))?;
        if has_source == 0 {
            conn.execute("ALTER TABLE dictionary ADD COLUMN source TEXT NOT NULL DEFAULT 'manual'", [])?;
        }
        // v0.3: usage counts rank vocabulary hints; corrections that don't sound alike wait for a repeat.
        let has_hits: i64 = conn.query_row("SELECT COUNT(*) FROM pragma_table_info('dictionary') WHERE name = 'hits'", [], |r| r.get(0))?;
        if has_hits == 0 {
            conn.execute("ALTER TABLE dictionary ADD COLUMN hits INTEGER NOT NULL DEFAULT 0", [])?;
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS learn_pending (phrase TEXT NOT NULL, replacement TEXT NOT NULL, seen INTEGER NOT NULL, updated_at TEXT NOT NULL, PRIMARY KEY (phrase, replacement));
             CREATE TABLE IF NOT EXISTS term_usage (term TEXT PRIMARY KEY, hits INTEGER NOT NULL, last_used TEXT NOT NULL);",
        )?;
        let has_transforms: i64 = conn.query_row("SELECT COUNT(*) FROM transforms", [], |r| r.get(0))?;
        if has_transforms == 0 {
            for (name, prompt) in BUILTIN_TRANSFORMS {
                conn.execute("INSERT INTO transforms (name, prompt, builtin) VALUES (?1, ?2, 1)", params![name, prompt])?;
            }
        }
        // v0.4: actions can copy their result instead of replacing the selection; meetings are
        // their own kind of note (they were told apart by a "Meeting" title prefix).
        let migration = conn.transaction()?;
        let version: i64 = migration.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let has_output: i64 = migration.query_row("SELECT COUNT(*) FROM pragma_table_info('transforms') WHERE name = 'output'", [], |r| r.get(0))?;
        let has_kind: i64 = migration.query_row("SELECT COUNT(*) FROM pragma_table_info('notes') WHERE name = 'kind'", [], |r| r.get(0))?;
        if has_output == 0 {
            migration.execute("ALTER TABLE transforms ADD COLUMN output TEXT NOT NULL DEFAULT 'replace'", [])?;
        }
        if has_kind == 0 {
            migration.execute_batch(
                "ALTER TABLE notes ADD COLUMN kind TEXT NOT NULL DEFAULT 'note';
                 UPDATE notes SET kind = 'meeting' WHERE title LIKE 'Meeting · %';",
            )?;
        }
        if version < 4 || has_output == 0 || has_kind == 0 {
            for (name, prompt) in BUILTIN_COPY_ACTIONS {
                // A user's own action with the same name keeps its name (and wins in command mode).
                let taken: i64 = migration.query_row("SELECT COUNT(*) FROM transforms WHERE lower(name) = lower(?1)", [name], |r| r.get(0))?;
                if taken == 0 {
                    migration.execute("INSERT INTO transforms (name, prompt, builtin, output) VALUES (?1, ?2, 1, 'copy')", params![name, prompt])?;
                }
            }
        }
        if version < 6 {
            for (name, retired) in RETIRED_PROMPTS {
                if let Some((_, current)) = BUILTIN_TRANSFORMS.iter().chain(BUILTIN_COPY_ACTIONS).find(|(n, _)| n == name) {
                    migration.execute("UPDATE transforms SET prompt = ?1 WHERE builtin = 1 AND prompt = ?2", params![current, retired])?;
                }
            }
            migration.pragma_update(None, "user_version", 6)?;
        }
        migration.commit()?;
        // v0.5: entries synced from the user's team library (`team.rs`), marked by the server's entry id.
        let migration = conn.transaction()?;
        for table in ["dictionary", "snippets", "transforms"] {
            let has_team: i64 = migration.query_row(&format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = 'team_id'"), [], |r| r.get(0))?;
            if has_team == 0 {
                migration.execute(&format!("ALTER TABLE {table} ADD COLUMN team_id TEXT"), [])?;
            }
            migration.execute(&format!("CREATE UNIQUE INDEX IF NOT EXISTS {table}_team_id ON {table}(team_id)"), [])?;
        }
        migration.execute_batch(
            "CREATE TABLE IF NOT EXISTS team_sync (id INTEGER PRIMARY KEY CHECK (id = 1), epoch INTEGER NOT NULL, team_id TEXT, team_name TEXT,
                version INTEGER NOT NULL, synced_at TEXT, error TEXT);
             INSERT OR IGNORE INTO team_sync (id, epoch, version) VALUES (1, 0, 0);
             CREATE TABLE IF NOT EXISTS team_skipped (id TEXT PRIMARY KEY);",
        )?;
        migration.commit()?;
        Ok(Db(Mutex::new(conn)))
    }

    /// The database as it is, never created, migrated or seeded: for `opstrellis mcp` and the CLI,
    /// which run beside the app. Read-only unless `writable`.
    pub fn open_existing(path: &Path, writable: bool) -> Result<Db> {
        use rusqlite::OpenFlags;
        let mode = if writable { OpenFlags::SQLITE_OPEN_READ_WRITE } else { OpenFlags::SQLITE_OPEN_READ_ONLY };
        let conn = Connection::open_with_flags(path, mode | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        if writable {
            conn.pragma_update(None, "secure_delete", "ON")?;
        }
        Ok(Db(Mutex::new(conn)))
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs `f` in one transaction: an error from `f` undoes everything it did.
    pub fn in_transaction<T>(&self, f: impl FnOnce(&rusqlite::Transaction) -> Result<T>) -> Result<T> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    // ---------- history ----------

    /// Records aggregates always; stores the item itself only when `keep` is true (privacy setting).
    pub fn add_history(&self, h: &HistoryItem, keep: bool) -> Result<Option<i64>> {
        let c = self.conn();
        if h.status == "ok" {
            let date = h.created_at.get(..10).unwrap_or_default();
            c.execute(
                "INSERT INTO daily_stats (date, category, dictations, words, audio_ms, fixes) VALUES (?1, ?2, 1, ?3, ?4, ?5)
                 ON CONFLICT(date, category) DO UPDATE SET dictations = dictations + 1, words = words + ?3,
                 audio_ms = audio_ms + ?4, fixes = fixes + ?5",
                params![date, h.category, h.words, h.audio_ms, h.fixes],
            )?;
        }
        if keep {
            c.execute(
                "INSERT INTO history (created_at, raw_text, text, app_name, category, provider, model, audio_ms, latency_ms, words, status, error, mode, fixes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![h.created_at, h.raw_text, h.text, h.app_name, h.category, h.provider, h.model, h.audio_ms,
                        h.latency_ms, h.words, h.status, h.error, h.mode, h.fixes],
            )?;
            return Ok(Some(c.last_insert_rowid()));
        }
        Ok(None)
    }

    pub fn get_history(&self, id: i64) -> Result<Option<HistoryItem>> {
        Ok(self.history_where("id = ?1", params![id])?.pop())
    }

    pub fn list_history(&self, query: Option<&str>, limit: i64, offset: i64) -> Result<Vec<HistoryItem>> {
        let pattern = format!("%{}%", query.unwrap_or("").trim());
        self.history_where("text LIKE ?1 OR raw_text LIKE ?1 ORDER BY id DESC LIMIT ?2 OFFSET ?3", params![pattern, limit, offset])
    }

    fn history_where(&self, clause: &str, p: impl rusqlite::Params) -> Result<Vec<HistoryItem>> {
        let c = self.conn();
        let mut stmt = c.prepare(&format!(
            "SELECT id, created_at, raw_text, text, app_name, category, provider, model, audio_ms, latency_ms, words, status, error, mode, fixes
             FROM history WHERE {clause}"
        ))?;
        let rows = stmt.query_map(p, |r| {
            Ok(HistoryItem {
                id: r.get(0)?,
                created_at: r.get(1)?,
                raw_text: r.get(2)?,
                text: r.get(3)?,
                app_name: r.get(4)?,
                category: r.get(5)?,
                provider: r.get(6)?,
                model: r.get(7)?,
                audio_ms: r.get(8)?,
                latency_ms: r.get(9)?,
                words: r.get(10)?,
                status: r.get(11)?,
                error: r.get(12)?,
                mode: r.get(13)?,
                fixes: r.get(14)?,
                has_audio: false,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Apps dictated into, most used first, with the category they were last classified as.
    /// One scan: with `MAX(id)`, SQLite takes the bare `category` from that (latest) row. A
    /// correlated subquery per app took ~3x longer on a large history, holding the database lock.
    pub fn list_apps(&self, limit: i64) -> Result<Vec<AppUse>> {
        let c = self.conn();
        let mut s = c.prepare(
            "SELECT app_name, category, COUNT(*) AS n, MAX(id) FROM history
             WHERE app_name IS NOT NULL AND app_name != '' GROUP BY app_name ORDER BY n DESC LIMIT ?1",
        )?;
        let rows = s.query_map([limit], |r| Ok(AppUse { app_name: r.get(0)?, category: r.get(1)?, count: r.get(2)? }))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn last_text(&self) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT text FROM history WHERE status = 'ok' ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
            .optional()?)
    }

    /// Replaces a history item's text; returns the previous text (None if the item doesn't exist).
    pub fn update_history_text(&self, id: i64, text: &str) -> Result<Option<String>> {
        let c = self.conn();
        let old: Option<String> = c.query_row("SELECT text FROM history WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        if old.is_some() {
            c.execute("UPDATE history SET text = ?1, words = ?2 WHERE id = ?3", params![text, text.split_whitespace().count() as i64, id])?;
        }
        Ok(old)
    }

    /// A slow AI request must not overwrite a correction or recreate a deleted history row.
    pub fn repolish_history_text(&self, item: &HistoryItem, text: &str) -> Result<bool> {
        Ok(self.conn().execute(
            "UPDATE history SET text = ?1, words = ?2 WHERE id = ?3 AND text = ?4 AND raw_text = ?5",
            params![text, text.split_whitespace().count() as i64, item.id, item.text, item.raw_text],
        )? == 1)
    }

    /// Saves a new transcription of `old` (from its kept recording), unless the row was changed or
    /// deleted meanwhile. A failed dictation that now worked counts in the aggregates once, on its
    /// own day; a dictation already counted keeps its count, as after a correction.
    pub fn retranscribed_history(&self, old: &HistoryItem, new: &HistoryItem) -> Result<bool> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        let changed = tx.execute(
            "UPDATE history SET raw_text = ?1, text = ?2, provider = ?3, model = ?4, words = ?5, fixes = ?6, status = 'ok', error = NULL
             WHERE id = ?7 AND status = ?8 AND text = ?9 AND raw_text = ?10",
            params![new.raw_text, new.text, new.provider, new.model, new.words, new.fixes, old.id, old.status, old.text, old.raw_text],
        )? == 1;
        if changed && old.status != "ok" {
            tx.execute(
                "INSERT INTO daily_stats (date, category, dictations, words, audio_ms, fixes) VALUES (?1, ?2, 1, ?3, ?4, ?5)
                 ON CONFLICT(date, category) DO UPDATE SET dictations = dictations + 1, words = words + ?3,
                 audio_ms = audio_ms + ?4, fixes = fixes + ?5",
                params![old.created_at.get(..10).unwrap_or_default(), old.category, new.words, old.audio_ms, new.fixes],
            )?;
        }
        tx.commit()?;
        Ok(changed)
    }

    /// The newest failed dictations, newest first.
    pub fn failed_dictation_ids(&self, limit: i64) -> Result<Vec<i64>> {
        let c = self.conn();
        let mut s = c.prepare("SELECT id FROM history WHERE status = 'error' AND mode = 'dictate' ORDER BY id DESC LIMIT ?1")?;
        let rows = s.query_map([limit], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn delete_history(&self, id: i64) -> Result<()> {
        let c = self.conn();
        c.execute("DELETE FROM history WHERE id = ?1", [id])?;
        checkpoint(&c)
    }

    /// Also drops pending corrections, which are fragments of history.
    pub fn clear_history(&self) -> Result<()> {
        let c = self.conn();
        c.execute_batch("DELETE FROM history; DELETE FROM learn_pending;")?;
        checkpoint(&c)
    }

    pub fn purge_history_older_than(&self, days: u32) -> Result<usize> {
        let cutoff = (Local::now() - Duration::days(days as i64)).to_rfc3339();
        let c = self.conn();
        let pending = c.execute("DELETE FROM learn_pending WHERE updated_at < ?1", [&cutoff])?;
        let deleted = c.execute("DELETE FROM history WHERE created_at < ?1", [cutoff])?;
        if pending + deleted > 0 {
            checkpoint(&c)?;
        }
        Ok(deleted)
    }

    pub fn stats(&self, today: NaiveDate) -> Result<Stats> {
        let c = self.conn();
        let mut stmt = c.prepare("SELECT date, category, dictations, words, audio_ms, fixes FROM daily_stats")?;
        let rows: Vec<(String, String, i64, i64, i64, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
            .collect::<Result<_, _>>()?;
        Ok(compute_stats(&rows, today))
    }

    // ---------- dictionary / snippets / transforms / notes ----------

    /// The dictionary dictation uses: the user's own entries, then the team's in use. The matcher
    /// keeps the first entry per phrase, so the order makes the user's own win.
    pub fn list_dictionary(&self) -> Result<Vec<DictEntry>> {
        Ok(self.list_dictionary_rows()?.into_iter().filter(|r| r.unused.is_none()).map(|r| r.entry).collect())
    }

    /// The user's own rows, then the team's, each newest first. Team entries fill the budget the
    /// user's own leave, oldest first; one whose phrase is a phrase or output of the user's own (as
    /// the matcher compares them) is shadowed; and if the team's make the matcher fail to build,
    /// none of them is used ("complex"), so the user's own still apply.
    pub fn list_dictionary_rows(&self) -> Result<Vec<DictRow>> {
        let (rows, (mut n, mut bytes)) = {
            let c = self.conn();
            let mut s = c.prepare("SELECT id, phrase, replacement, created_at, source FROM dictionary ORDER BY id")?;
            let rows: Vec<DictRow> = s
                .query_map([], |r| {
                    let source: String = r.get(4)?;
                    let entry = DictEntry { id: r.get(0)?, phrase: r.get(1)?, replacement: r.get(2)?, created_at: r.get(3)?, learned: source == "learned" };
                    Ok(DictRow { entry, source, unused: None })
                })?
                .collect::<Result<_, _>>()?;
            (rows, dictionary_usage(&c, None)?)
        };
        use crate::terms::norm;
        let unused = team_unused(
            &rows,
            |r| r.source == "team",
            |r| vec![norm(&r.entry.phrase), norm(r.entry.replacement.as_deref().unwrap_or(&r.entry.phrase))],
            |r| norm(&r.entry.phrase),
            |r| {
                n += 1;
                bytes += r.entry.phrase.len() + r.entry.replacement.as_ref().map_or(0, String::len);
                n <= DICT_MAX_ENTRIES && bytes <= DICT_MAX_BYTES
            },
        );
        let mut rows: Vec<DictRow> = rows.into_iter().zip(unused).map(|(r, unused)| DictRow { unused, ..r }).collect();
        let team_in_use = |r: &DictRow| r.source == "team" && r.unused.is_none();
        if rows.iter().any(team_in_use) {
            let mut in_use: Vec<DictEntry> = rows.iter().filter(|r| r.unused.is_none() && r.source != "team").map(|r| r.entry.clone()).collect();
            in_use.extend(rows.iter().filter(|r| team_in_use(r)).map(|r| r.entry.clone()));
            if !matcher_builds(&in_use) {
                log::warn!("the team's dictionary entries make the dictionary too complex to match; only the user's own are used");
                for r in rows.iter_mut().filter(|r| r.source == "team" && r.unused.is_none()) {
                    r.unused = Some("complex");
                }
            }
        }
        rows.reverse();
        rows.sort_by_key(|r| r.source == "team");
        Ok(rows)
    }

    /// Adds learned corrections. Entries the user created by hand are never overridden; an earlier
    /// learned entry for the same phrase is updated, and one whose output was corrected again
    /// (Karl -> Carl, then Carl -> Karol) is repointed. A correction that doesn't sound like a
    /// misrecognition is only learned once the user makes it a second time.
    pub fn learn(&self, corrections: Vec<crate::learn::Correction>) -> Result<Learned> {
        // Phrases match like text::phrase_regex: case-insensitive (Unicode), spaces or hyphens between words.
        let key = |s: &str| s.to_lowercase().split(|ch: char| ch.is_whitespace() || ch == '-').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
        let mut c = self.conn();
        let tx = c.transaction()?;
        let now = now_rfc3339();
        // A fix not repeated within 30 days is forgotten.
        tx.execute("DELETE FROM learn_pending WHERE updated_at < ?1", [(Local::now() - Duration::days(30)).to_rfc3339()])?;
        let (mut added, mut pending, mut undo, mut full) = (Vec::new(), Vec::new(), Vec::new(), false);
        for k in corrections {
            // Team entries are read-only and never block learning: the user's own entry then wins over one.
            let rows: Vec<(i64, String, Option<String>, bool)> = tx
                .prepare("SELECT id, phrase, replacement, source = 'learned' FROM dictionary WHERE source != 'team' ORDER BY id")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<Result<_, _>>()?;
            let same: Vec<_> = rows.iter().filter(|r| key(&r.1) == key(&k.phrase)).collect();
            // A hand-made entry wins; a fix already learned has nothing left to learn.
            if same.iter().any(|r| !r.3 || (r.2 == k.replacement && (k.replacement.is_some() || r.1 == k.phrase))) {
                continue;
            }
            if let (false, Some(r)) = (crate::learn::sounds_alike(&k), &k.replacement) {
                let pk = (key(&k.phrase), r.as_str());
                let seen: i64 = tx.query_row(
                    "INSERT INTO learn_pending (phrase, replacement, seen, updated_at) VALUES (?1, ?2, 1, ?3)
                     ON CONFLICT DO UPDATE SET seen = seen + 1, updated_at = ?3 RETURNING seen",
                    params![pk.0, pk.1, now],
                    |row| row.get(0),
                )?;
                if seen < 2 {
                    pending.push(k);
                    continue;
                }
                tx.execute("DELETE FROM learn_pending WHERE phrase = ?1 AND replacement = ?2", params![pk.0, pk.1])?;
            }
            let target = k.replacement.as_deref().unwrap_or(&k.phrase);
            for (id, phrase, _, _) in rows.iter().filter(|r| r.3 && r.2.as_deref().is_some_and(|r| key(r) == key(&k.phrase))) {
                if phrase == target {
                    tracked(&tx, &mut undo, *id, "DELETE FROM dictionary WHERE id = ?1", params![id])?; // corrected back to its own phrase
                } else {
                    tracked(&tx, &mut undo, *id, "UPDATE dictionary SET replacement = ?1 WHERE id = ?2", params![target, id])?;
                }
            }
            let id = match same.first() {
                Some((_, phrase, replacement, _)) if *phrase == k.phrase && *replacement == k.replacement => continue,
                Some((id, ..)) => {
                    tracked(&tx, &mut undo, *id, "UPDATE dictionary SET phrase = ?1, replacement = ?2 WHERE id = ?3", params![k.phrase, k.replacement, id])?;
                    *id
                }
                None => {
                    let (entries, bytes) = dictionary_usage(&tx, None)?;
                    if entries >= DICT_MAX_ENTRIES || bytes + k.phrase.len() + k.replacement.as_ref().map_or(0, String::len) > DICT_MAX_BYTES {
                        full = true;
                        continue;
                    }
                    tx.execute(
                        "INSERT INTO dictionary (phrase, replacement, created_at, source) VALUES (?1, ?2, ?3, 'learned')",
                        params![k.phrase, k.replacement, now],
                    )?;
                    let id = tx.last_insert_rowid();
                    undo.push(UndoRow { id, before: None, after: row_state(&tx, id)? });
                    id
                }
            };
            added.push(DictEntry { id: Some(id), phrase: k.phrase, replacement: k.replacement, created_at: Some(now.clone()), learned: true });
        }
        tx.commit()?;
        Ok(Learned { added, pending, undo, full })
    }

    /// Reverts what `learn` changed, newest first. Rows changed since, or turned into hand-made
    /// entries, are left alone.
    pub fn undo_learn(&self, rows: &[UndoRow]) -> Result<()> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        for r in rows.iter().rev() {
            let manual: bool = tx.query_row("SELECT source IN ('manual', 'team') FROM dictionary WHERE id = ?1", [r.id], |x| x.get(0)).optional()?.unwrap_or(false);
            if manual || row_state(&tx, r.id)? != r.after {
                continue;
            }
            match (&r.before, &r.after) {
                (None, _) => tx.execute("DELETE FROM dictionary WHERE id = ?1", [r.id])?,
                (Some((p, rep)), Some(_)) => tx.execute("UPDATE dictionary SET phrase = ?1, replacement = ?2 WHERE id = ?3", params![p, rep, r.id])?,
                (Some((p, rep)), None) => tx.execute(
                    "INSERT INTO dictionary (id, phrase, replacement, created_at, source) VALUES (?1, ?2, ?3, ?4, 'learned')",
                    params![r.id, p, rep, now_rfc3339()],
                )?,
            };
        }
        tx.commit()?;
        Ok(())
    }

    /// Counts what a dictation used: dictionary entries that matched and built-in terms it contains.
    pub fn record_usage(&self, entries: &[i64], terms: &[impl AsRef<str>]) -> Result<()> {
        if entries.is_empty() && terms.is_empty() {
            return Ok(());
        }
        let mut c = self.conn();
        let tx = c.transaction()?;
        for id in entries {
            tx.execute("UPDATE dictionary SET hits = hits + 1 WHERE id = ?1", [id])?;
        }
        let now = now_rfc3339();
        for t in terms {
            tx.execute(
                "INSERT INTO term_usage (term, hits, last_used) VALUES (?1, 1, ?2)
                 ON CONFLICT(term) DO UPDATE SET hits = hits + 1, last_used = ?2",
                params![t.as_ref(), now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Words to send as transcription hints: the user's dictionary, most used first, then up to
    /// `terms` terms of the packs in use: the ones they say most (`in_use` leaves out those of
    /// packs turned off since), then `pack_terms` in order.
    pub fn vocabulary(&self, limit: usize, terms: usize, in_use: impl Fn(&str) -> bool, pack_terms: &[String]) -> Result<Vec<String>> {
        let unused: std::collections::HashSet<i64> = self.list_dictionary_rows()?.into_iter().filter(|r| r.unused.is_some()).filter_map(|r| r.entry.id).collect();
        let c = self.conn();
        let mut out: Vec<String> = c
            .prepare("SELECT id, COALESCE(replacement, phrase) FROM dictionary WHERE length(COALESCE(replacement, phrase)) <= 60 ORDER BY hits DESC, id DESC")?
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .filter(|r| !matches!(r, Ok((id, _)) if unused.contains(id)))
            .take(limit)
            .map(|r| r.map(|(_, w)| w))
            .collect::<Result<_, _>>()?;
        let end = (out.len() + terms).min(limit);
        let add = |out: &mut Vec<String>, t: &str| {
            if !out.iter().any(|w| w.eq_ignore_ascii_case(t)) {
                out.push(t.to_string());
            }
        };
        let mut used = c.prepare("SELECT term FROM term_usage ORDER BY hits DESC, last_used DESC")?;
        for t in used.query_map([], |r| r.get::<_, String>(0))? {
            if out.len() >= end {
                break;
            }
            let t = t?;
            if in_use(&t) {
                add(&mut out, &t);
            }
        }
        for t in pack_terms {
            if out.len() >= end {
                break;
            }
            add(&mut out, t);
        }
        Ok(out)
    }

    pub fn upsert_dictionary(&self, mut e: DictEntry) -> Result<DictEntry> {
        e.phrase = e.phrase.trim().to_string();
        e.replacement = e.replacement.map(|r| r.trim().to_string()).filter(|r| !r.is_empty());
        anyhow::ensure!(!e.phrase.is_empty(), "Word or phrase is required");
        anyhow::ensure!(plain_line(&e.phrase) && e.replacement.as_deref().is_none_or(plain_line), "Words and replacements can't contain control characters");
        let c = self.conn();
        ensure_own(&c, "dictionary", e.id)?;
        let (others, bytes) = dictionary_usage(&c, e.id)?;
        anyhow::ensure!(
            others < DICT_MAX_ENTRIES && bytes + e.phrase.len() + e.replacement.as_ref().map_or(0, String::len) <= DICT_MAX_BYTES,
            "The dictionary is full: it holds up to {DICT_MAX_ENTRIES} entries and {DICT_MAX_BYTES} bytes of words and replacements. Delete some first."
        );
        let now = now_rfc3339();
        match e.id {
            Some(id) => {
                c.execute("UPDATE dictionary SET phrase = ?1, replacement = ?2, source = 'manual' WHERE id = ?3", params![e.phrase, e.replacement, id])?;
            }
            None => {
                c.execute("INSERT INTO dictionary (phrase, replacement, created_at) VALUES (?1, ?2, ?3)", params![e.phrase, e.replacement, now])?;
                e.id = Some(c.last_insert_rowid());
                e.created_at = Some(now);
            }
        }
        Ok(e)
    }

    /// Adds `phrase` as a vocabulary hint (no replacement), checked like an entry typed in, and marks
    /// it as coming from `source`. None if the dictionary already has the phrase (trimmed, any case).
    pub fn add_hint(&self, phrase: &str, source: &str) -> Result<Option<DictEntry>> {
        // One write transaction, so the app can't add the same phrase or fill the budget in between.
        self.conn().execute_batch("BEGIN IMMEDIATE")?;
        let added = (|| {
            let key = phrase.trim().to_lowercase();
            if self.list_dictionary()?.iter().any(|e| e.phrase.trim().to_lowercase() == key) {
                return Ok(None);
            }
            let e = self.upsert_dictionary(DictEntry { id: None, phrase: phrase.into(), replacement: None, created_at: None, learned: false })?;
            self.conn().execute("UPDATE dictionary SET source = ?1 WHERE id = ?2", params![source, e.id])?;
            Ok(Some(e))
        })();
        let end = if added.is_ok() { "COMMIT" } else { "ROLLBACK" };
        if let Err(e) = self.conn().execute_batch(end) {
            let _ = self.conn().execute_batch("ROLLBACK");
            return Err(e.into());
        }
        added
    }

    pub fn delete_dictionary(&self, id: i64) -> Result<()> {
        let c = self.conn();
        ensure_own(&c, "dictionary", Some(id))?;
        c.execute("DELETE FROM dictionary WHERE id = ?1", [id])?;
        Ok(())
    }

    /// The snippets dictation uses: the user's own, then the team's in use. Snippets expand in this
    /// order and the first match wins, so a team expansion can never trigger one of the user's own.
    pub fn list_snippets(&self) -> Result<Vec<Snippet>> {
        Ok(self.list_snippet_rows()?.into_iter().filter(|r| r.unused.is_none()).map(|r| r.entry).collect())
    }

    /// The user's own snippets, then the team's, each newest first.
    pub fn list_snippet_rows(&self) -> Result<Vec<LibraryRow<Snippet>>> {
        let c = self.conn();
        let mut s = c.prepare("SELECT id, trigger, expansion, created_at, team_id IS NOT NULL FROM snippets ORDER BY id")?;
        let rows: Vec<(Snippet, bool)> = s
            .query_map([], |r| Ok((Snippet { id: r.get(0)?, trigger: r.get(1)?, expansion: r.get(2)?, created_at: r.get(3)? }, r.get(4)?)))?
            .collect::<Result<_, _>>()?;
        let mut used = 0;
        let unused = team_unused(&rows, |r| r.1, |r| vec![trigger_key(&r.0.trigger)], |r| trigger_key(&r.0.trigger), |_| {
            used += 1;
            used <= TEAM_MAX_SNIPPETS
        });
        let mut rows: Vec<_> = rows.into_iter().zip(unused).map(|((entry, team), unused)| LibraryRow { entry, team, unused }).rev().collect();
        rows.sort_by_key(|r| r.team);
        Ok(rows)
    }

    pub fn upsert_snippet(&self, mut s: Snippet) -> Result<Snippet> {
        s.trigger = s.trigger.trim().to_string();
        anyhow::ensure!(!s.trigger.is_empty() && !s.expansion.trim().is_empty(), "Trigger and expansion are required");
        anyhow::ensure!(plain_line(&s.trigger) && plain_text(&s.expansion), "Triggers can't contain control characters, and expansions only newlines and tabs");
        let c = self.conn();
        ensure_own(&c, "snippets", s.id)?;
        let now = now_rfc3339();
        match s.id {
            Some(id) => {
                c.execute("UPDATE snippets SET trigger = ?1, expansion = ?2 WHERE id = ?3", params![s.trigger, s.expansion, id])?;
            }
            None => {
                c.execute("INSERT INTO snippets (trigger, expansion, created_at) VALUES (?1, ?2, ?3)", params![s.trigger, s.expansion, now])?;
                s.id = Some(c.last_insert_rowid());
                s.created_at = Some(now);
            }
        }
        Ok(s)
    }

    pub fn delete_snippet(&self, id: i64) -> Result<()> {
        let c = self.conn();
        ensure_own(&c, "snippets", Some(id))?;
        c.execute("DELETE FROM snippets WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Built-in and the user's own actions, never the team's: these are the ones an auto action,
    /// a backup or a lookup by name may use. Command mode asks `team_action` only when none matches.
    pub fn list_transforms(&self) -> Result<Vec<Transform>> {
        Ok(self.list_transform_rows()?.into_iter().filter(|r| !r.team).map(|r| r.entry).collect())
    }

    /// The team action in use whose name is exactly `spoken` (trimmed and lowercased by the caller).
    pub fn team_action(&self, spoken: &str) -> Result<Option<Transform>> {
        Ok(self.list_transform_rows()?.into_iter().find(|r| r.team && r.unused.is_none() && r.entry.name.trim().to_lowercase() == spoken).map(|r| r.entry))
    }

    /// Every action: built-in, the user's own, then the team's, each oldest first.
    pub fn list_transform_rows(&self) -> Result<Vec<LibraryRow<Transform>>> {
        let c = self.conn();
        let mut s = c.prepare("SELECT id, name, prompt, builtin, output, team_id IS NOT NULL AS team FROM transforms ORDER BY builtin DESC, team, id")?;
        let rows: Vec<(Transform, bool)> = s
            .query_map([], |r| Ok((Transform { id: r.get(0)?, name: r.get(1)?, prompt: r.get(2)?, builtin: r.get(3)?, output: r.get(4)? }, r.get(5)?)))?
            .collect::<Result<_, _>>()?;
        let mut used = 0;
        let key = |r: &(Transform, bool)| r.0.name.trim().to_lowercase();
        let unused = team_unused(&rows, |r| r.1, |r| vec![key(r)], key, |_| {
            used += 1;
            used <= TEAM_MAX_ACTIONS
        });
        Ok(rows.into_iter().zip(unused).map(|((entry, team), unused)| LibraryRow { entry, team, unused }).collect())
    }

    pub fn upsert_transform(&self, mut t: Transform) -> Result<Transform> {
        anyhow::ensure!(!t.name.trim().is_empty() && !t.prompt.trim().is_empty(), "Name and prompt are required");
        anyhow::ensure!(plain_line(&t.name) && plain_text(&t.prompt), "Names can't contain control characters, and prompts only newlines and tabs");
        anyhow::ensure!(matches!(t.output.as_str(), "replace" | "copy"), "Unknown output \"{}\"", t.output);
        let c = self.conn();
        ensure_own(&c, "transforms", t.id)?;
        match t.id {
            Some(id) => {
                c.execute("UPDATE transforms SET name = ?1, prompt = ?2, output = ?3 WHERE id = ?4", params![t.name, t.prompt, t.output, id])?;
            }
            None => {
                c.execute("INSERT INTO transforms (name, prompt, builtin, output) VALUES (?1, ?2, 0, ?3)", params![t.name, t.prompt, t.output])?;
                t.id = Some(c.last_insert_rowid());
            }
        }
        Ok(t)
    }

    pub fn delete_transform(&self, id: i64) -> Result<()> {
        let c = self.conn();
        ensure_own(&c, "transforms", Some(id))?;
        c.execute("DELETE FROM transforms WHERE id = ?1 AND builtin = 0", [id])?;
        Ok(())
    }

    /// Notes of one kind ("note" or "meeting"), or all of them.
    pub fn list_notes(&self, kind: Option<&str>) -> Result<Vec<Note>> {
        let c = self.conn();
        let mut s = c.prepare("SELECT id, title, body, updated_at, kind FROM notes WHERE ?1 IS NULL OR kind = ?1 ORDER BY updated_at DESC")?;
        let rows = s.query_map([kind], |r| Ok(Note { id: r.get(0)?, title: r.get(1)?, body: r.get(2)?, updated_at: r.get(3)?, kind: r.get(4)? }))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get_note(&self, id: i64) -> Result<Option<Note>> {
        Ok(self
            .conn()
            .query_row("SELECT id, title, body, updated_at, kind FROM notes WHERE id = ?1", [id], |r| {
                Ok(Note { id: r.get(0)?, title: r.get(1)?, body: r.get(2)?, updated_at: r.get(3)?, kind: r.get(4)? })
            })
            .optional()?)
    }

    pub fn upsert_note(&self, mut n: Note) -> Result<Note> {
        let c = self.conn();
        let now = now_rfc3339();
        match n.id {
            Some(id) => {
                c.execute("UPDATE notes SET title = ?1, body = ?2, updated_at = ?3 WHERE id = ?4", params![n.title, n.body, now, id])?;
            }
            None => {
                c.execute("INSERT INTO notes (title, body, updated_at, kind) VALUES (?1, ?2, ?3, ?4)", params![n.title, n.body, now, n.kind])?;
                n.id = Some(c.last_insert_rowid());
            }
        }
        n.updated_at = Some(now);
        Ok(n)
    }

    pub fn delete_note(&self, id: i64) -> Result<()> {
        let c = self.conn();
        c.execute("DELETE FROM notes WHERE id = ?1", [id])?;
        checkpoint(&c)
    }

    /// Once per database: rebuilds it, so text deleted before `secure_delete` was on (freed pages
    /// keep their old content) is gone from the file. Returns whether it ran.
    ///
    /// On a connection of its own, so the app's (and a dictation starting meanwhile) isn't held. With
    /// WAL the app keeps reading from its snapshot; only a write waits, within the 5 s busy timeout,
    /// which is why a file over `VACUUM_MAX_BYTES` (seconds to rebuild) is skipped, and logged.
    pub fn vacuum_once(path: &Path) -> Result<bool> {
        Self::vacuum_once_within(path, VACUUM_MAX_BYTES)
    }

    fn vacuum_once_within(path: &Path, max_bytes: u64) -> Result<bool> {
        const TASK: &str = "vacuum-secure-delete";
        let c = Connection::open(path)?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        c.execute("CREATE TABLE IF NOT EXISTS maintenance (task TEXT PRIMARY KEY)", [])?;
        if c.query_row("SELECT COUNT(*) FROM maintenance WHERE task = ?1", [TASK], |r| r.get::<_, i64>(0))? > 0 {
            return Ok(false);
        }
        let size = std::fs::metadata(path)?.len();
        if size > max_bytes {
            log::warn!("not compacting the database ({size} bytes): text deleted by earlier versions may remain in its free space");
            return Ok(false);
        }
        // The rebuild's temporary copy stays in memory, not in a temp file on disk.
        c.pragma_update(None, "temp_store", "MEMORY")?;
        let vacuumed = c.execute_batch("VACUUM");
        c.pragma_update(None, "temp_store", "DEFAULT")?;
        vacuumed?;
        c.execute("INSERT INTO maintenance (task) VALUES (?1)", [TASK])?;
        checkpoint(&c)?;
        Ok(true)
    }

    // ---------- usage outbox ----------

    pub fn queue_usage(&self, e: &UsageEvent) -> Result<()> {
        self.conn()
            .execute("INSERT OR IGNORE INTO usage_outbox (id, json) VALUES (?1, ?2)", params![e.id, serde_json::to_string(e)?])?;
        Ok(())
    }

    pub fn pending_usage(&self, limit: i64) -> Result<Vec<(String, serde_json::Value)>> {
        let c = self.conn();
        let mut s = c.prepare("SELECT id, json FROM usage_outbox LIMIT ?1")?;
        let rows = s.query_map([limit], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows
            .filter_map(|r| r.ok())
            .filter_map(|(id, j)| serde_json::from_str(&j).ok().map(|v| (id, v)))
            .collect())
    }

    pub fn clear_usage(&self) -> Result<()> {
        self.conn().execute("DELETE FROM usage_outbox", [])?;
        Ok(())
    }

    pub fn ack_usage(&self, ids: &[String]) -> Result<()> {
        let c = self.conn();
        for id in ids {
            c.execute("DELETE FROM usage_outbox WHERE id = ?1", [id])?;
        }
        Ok(())
    }
}

/// Above this a one-time rebuild could make a dictation's history write wait past its busy timeout.
const VACUUM_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// Copies the WAL into the database file and empties it, so deleted rows' old pages don't linger there.
fn checkpoint(c: &Connection) -> Result<()> {
    c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    Ok(())
}

/// rows: (date, category, dictations, words, audio_ms, fixes)
fn compute_stats(rows: &[(String, String, i64, i64, i64, i64)], today: NaiveDate) -> Stats {
    let mut by_day: BTreeMap<NaiveDate, i64> = BTreeMap::new();
    let mut by_cat: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    let (mut words, mut dictations, mut audio_ms, mut fixes) = (0, 0, 0, 0);
    for (date, cat, n, w, a, f) in rows {
        words += w;
        dictations += n;
        audio_ms += a;
        fixes += f;
        if let Ok(d) = NaiveDate::parse_from_str(date, "%Y-%m-%d") {
            *by_day.entry(d).or_default() += w;
        }
        let e = by_cat.entry(cat.clone()).or_default();
        e.0 += n;
        e.1 += w;
    }

    // Longest run of consecutive active days, and the run ending today (or yesterday, so the streak
    // isn't shown as broken before the user dictates today).
    let (mut longest, mut run, mut prev) = (0i64, 0i64, None::<NaiveDate>);
    for d in by_day.keys() {
        run = if prev.map(|p| *d - p == Duration::days(1)).unwrap_or(false) { run + 1 } else { 1 };
        longest = longest.max(run);
        prev = Some(*d);
    }
    let mut streak = 0;
    let mut day = if by_day.contains_key(&today) { today } else { today - Duration::days(1) };
    while by_day.contains_key(&day) {
        streak += 1;
        day -= Duration::days(1);
    }

    let wpm = if audio_ms > 0 { (words as f64 / (audio_ms as f64 / 60_000.0)).round() as i64 } else { 0 };
    let since = today - Duration::days(365);
    Stats {
        total_words: words,
        total_dictations: dictations,
        wpm,
        fixes,
        streak,
        longest_streak: longest,
        days: by_day
            .range(since..)
            .map(|(d, w)| DayWords { date: d.format("%Y-%m-%d").to_string(), words: *w })
            .collect(),
        categories: by_cat
            .into_iter()
            .map(|(category, (count, words))| CategoryStat { category, count, words })
            .collect(),
    }
}

/// Local time, so the date prefix (used for daily stats and streaks) is the user's calendar day.
pub fn now_rfc3339() -> String {
    Local::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> Db {
        let dir = std::env::temp_dir().join(format!("opstrellis-db-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(&dir.join("t.db")).unwrap()
    }

    #[test]
    fn deleted_history_is_gone_from_the_files() {
        let dir = std::env::temp_dir().join(format!("opstrellis-db-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let db = Db::open(&path).unwrap();
        let on_disk = |needle: &str| {
            ["t.db", "t.db-wal"].iter().any(|f| std::fs::read(dir.join(f)).unwrap_or_default().windows(needle.len()).any(|w| w == needle.as_bytes()))
        };
        let item = |text: &str, created_at: &str| HistoryItem { text: text.into(), raw_text: text.into(), created_at: created_at.into(), status: "ok".into(), ..Default::default() };
        db.add_history(&item("zebra quartz secret one", &now_rfc3339()), true).unwrap();
        db.add_history(&item("zebra quartz secret old", "2001-01-01T00:00:00+00:00"), true).unwrap();
        let single = db.add_history(&item("zebra quartz secret single", &now_rfc3339()), true).unwrap().unwrap();
        let note = db.upsert_note(Note { id: None, title: "t".into(), body: "zebra quartz meeting".into(), updated_at: None, kind: "meeting".into() }).unwrap();
        assert!(on_disk("zebra quartz secret one") && on_disk("zebra quartz secret old") && on_disk("zebra quartz secret single") && on_disk("zebra quartz meeting"));
        db.delete_history(single).unwrap();
        db.delete_note(note.id.unwrap()).unwrap();
        assert!(!on_disk("zebra quartz secret single") && !on_disk("zebra quartz meeting"), "single deletes too");
        assert_eq!(db.purge_history_older_than(30).unwrap(), 1);
        assert!(!on_disk("zebra quartz secret old") && on_disk("zebra quartz secret one"));
        db.clear_history().unwrap();
        assert!(!on_disk("zebra quartz secret one"));
        assert_eq!(db.stats(chrono::Local::now().date_naive()).unwrap().total_dictations, 3, "Insights totals are kept");
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn text_deleted_before_secure_delete_is_compacted_away_once() {
        let dir = std::env::temp_dir().join(format!("opstrellis-db-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        drop(Db::open(&path).unwrap());
        {
            // As an earlier version left it: deleted without secure_delete.
            let c = Connection::open(&path).unwrap();
            c.pragma_update(None, "secure_delete", "OFF").unwrap();
            c.execute("INSERT INTO notes (title, body, updated_at) VALUES ('t', ?1, 'x')", [format!("walrus ledger {}", "pad ".repeat(2000))]).unwrap();
            c.execute("DELETE FROM notes", []).unwrap();
            c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())).unwrap();
        }
        let in_file = || std::fs::read(&path).unwrap().windows(13).any(|w| w == b"walrus ledger");
        assert!(in_file(), "an old-style delete leaves the text in the file");
        let db = Db::open(&path).unwrap();
        assert!(!Db::vacuum_once_within(&path, 10).unwrap() && in_file(), "a large database is skipped");
        assert!(Db::vacuum_once(&path).unwrap(), "runs beside the app's open connection");
        assert!(!in_file());
        assert!(!db.list_notes(None).unwrap().iter().any(|n| n.body.contains("walrus")), "the app's connection still works");
        assert!(!Db::vacuum_once(&path).unwrap(), "only once");
        drop(db);
        assert!(!Db::vacuum_once(&path).unwrap(), "also across launches");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn usage_outbox_can_be_cleared() {
        let db = temp_db();
        let ev = UsageEvent { id: "a".into(), provider: "local".into(), model: "m".into(), mode: "local".into(), audio_seconds: 1.0, words: 3, app_category: "other".into(), created_at: now_rfc3339() };
        db.queue_usage(&ev).unwrap();
        assert_eq!(db.pending_usage(10).unwrap().len(), 1);
        db.clear_usage().unwrap();
        assert!(db.pending_usage(10).unwrap().is_empty());
    }

    #[test]
    fn undo_restores_what_learning_changed() {
        use crate::learn::Correction;
        let fix = |p: &str, r: &str| vec![Correction { phrase: p.into(), replacement: Some(r.into()) }];
        let dict = |db: &Db| db.list_dictionary().unwrap().into_iter().map(|d| (d.phrase, d.replacement)).collect::<Vec<_>>();
        let db = temp_db();
        db.learn(fix("Karl", "Karol")).unwrap();

        // An update is reverted, not deleted.
        let l = db.learn(fix("Karl", "Carol")).unwrap();
        db.undo_learn(&l.undo).unwrap();
        assert_eq!(dict(&db), vec![("Karl".into(), Some("Karol".into()))]);

        // A repointed guess is restored too, and the new entry removed.
        let l = db.learn(fix("Karol", "Carol")).unwrap();
        assert_eq!(dict(&db), vec![("Karol".into(), Some("Carol".into())), ("Karl".into(), Some("Carol".into()))]);
        db.undo_learn(&l.undo).unwrap();
        assert_eq!(dict(&db), vec![("Karl".into(), Some("Karol".into()))]);

        // A guess deleted by correcting it back to its phrase comes back.
        let l = db.learn(fix("Karol", "Karl")).unwrap();
        assert_eq!(dict(&db), vec![("Karol".into(), Some("Karl".into()))]);
        db.undo_learn(&l.undo).unwrap();
        assert_eq!(dict(&db), vec![("Karl".into(), Some("Karol".into()))]);

        // An entry edited by hand since is left alone.
        let l = db.learn(fix("Jon", "John")).unwrap();
        let id = l.added[0].id;
        db.upsert_dictionary(DictEntry { id, phrase: "Jon".into(), replacement: Some("Johnny".into()), created_at: None, learned: false }).unwrap();
        db.undo_learn(&l.undo).unwrap();
        assert!(dict(&db).contains(&("Jon".into(), Some("Johnny".into()))));
    }

    #[test]
    fn pending_fixes_are_history_and_expire() {
        use crate::learn::Correction;
        let swap = || vec![Correction { phrase: "Bob".into(), replacement: Some("Alice".into()) }];
        let db = temp_db();
        db.learn(swap()).unwrap();
        db.clear_history().unwrap();
        assert!(db.learn(swap()).unwrap().added.is_empty(), "clearing history forgets pending fixes");
        let old = (Local::now() - Duration::days(31)).to_rfc3339();
        db.conn().execute("UPDATE learn_pending SET updated_at = ?1", [old]).unwrap();
        assert!(db.learn(swap()).unwrap().added.is_empty(), "a fix not repeated within 30 days is forgotten");
        assert_eq!(db.learn(swap()).unwrap().added.len(), 1);
        let again = db.learn(swap()).unwrap();
        assert!(again.added.is_empty() && again.pending.is_empty(), "already learned: {again:?}");
    }

    fn row(d: &str, w: i64) -> (String, String, i64, i64, i64, i64) {
        (d.into(), "other".into(), 1, w, 60_000, 1)
    }

    #[test]
    fn streaks_and_wpm() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 14).unwrap();
        let rows = vec![
            row("2026-10-01", 10),
            row("2026-10-02", 10),
            row("2026-10-03", 10),
            row("2026-10-04", 10),
            row("2026-10-12", 20),
            row("2026-10-13", 20),
        ];
        let s = compute_stats(&rows, today);
        assert_eq!(s.longest_streak, 4);
        assert_eq!(s.streak, 2, "streak continues through yesterday");
        assert_eq!(s.total_words, 80);
        assert_eq!(s.wpm, 13); // 80 words / 6 minutes
        assert_eq!(s.fixes, 6);
        let s2 = compute_stats(&rows, NaiveDate::from_ymd_opt(2026, 10, 16).unwrap());
        assert_eq!(s2.streak, 0);
    }

    /// A database from before actions had outputs and meetings had a kind.
    #[test]
    fn migrates_outputs_and_note_kinds() {
        let dir = std::env::temp_dir().join(format!("opstrellis-db-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE transforms (id INTEGER PRIMARY KEY, name TEXT NOT NULL, prompt TEXT NOT NULL, builtin INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT NOT NULL, body TEXT NOT NULL, updated_at TEXT NOT NULL);
                 INSERT INTO transforms (name, prompt, builtin) VALUES ('Polish', 'p', 1), ('reply', 'My own reply prompt', 0);
                 INSERT INTO notes (title, body, updated_at) VALUES ('Meeting · Oct 1, 10:00', 'm', 'x'), ('Meeting prep', 'n', 'x'), ('Groceries', 'g', 'x');",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let t = db.list_transforms().unwrap();
        assert!(t.iter().filter(|t| t.name != "Summarize").all(|t| t.output == "replace"), "existing actions keep replacing");
        // The user's own "reply" keeps its name; only the missing built-in is added.
        assert_eq!(t.iter().filter(|t| t.name.eq_ignore_ascii_case("reply")).count(), 1);
        assert_eq!(t.iter().find(|t| t.name == "Summarize").map(|t| t.output.as_str()), Some("copy"));
        let titles = |kind| db.list_notes(Some(kind)).unwrap().into_iter().map(|n| n.title).collect::<Vec<_>>();
        assert_eq!(titles("meeting"), vec!["Meeting · Oct 1, 10:00"]);
        assert_eq!(titles("note").len(), 2, "\"Meeting prep\" is a note");
        // Re-opening doesn't seed again.
        drop(db);
        assert_eq!(Db::open(&path).unwrap().list_transforms().unwrap().len(), t.len());
        assert!(db_upsert_rejects_unknown_output(&Db::open(&path).unwrap()));
    }

    #[test]
    fn retired_builtin_prompts_are_replaced_unless_edited() {
        // One entry per earlier version of a built-in prompt: dropping one would leave its users on it.
        let names: Vec<&str> = RETIRED_PROMPTS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["Polish", "Prompt engineer", "Make formal", "Shorten", "Bullet points", "Reply", "Summarize", "Prompt engineer", "Bullet points", "Summarize"]);
        let dir = std::env::temp_dir().join(format!("opstrellis-prompts-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("db");
        drop(Db::open(&path).unwrap());
        let current = |name: &str| BUILTIN_TRANSFORMS.iter().chain(BUILTIN_COPY_ACTIONS).find(|(n, _)| *n == name).unwrap().1;
        for version in [4, 5] {
            for (name, retired) in RETIRED_PROMPTS {
                assert_ne!(*retired, current(name), "{name}");
                let conn = Connection::open(&path).unwrap();
                assert_eq!(conn.execute("UPDATE transforms SET prompt = ?1 WHERE builtin = 1 AND name = ?2", params![retired, name]).unwrap(), 1, "{name}");
                conn.pragma_update(None, "user_version", version).unwrap();
                drop(conn);
                let actions = Db::open(&path).unwrap().list_transforms().unwrap();
                assert_eq!(actions.iter().find(|t| t.name == *name).unwrap().prompt, current(name), "v{version}: {retired}");
            }
            let conn = Connection::open(&path).unwrap();
            conn.execute("UPDATE transforms SET prompt = 'My polish' WHERE name = 'Polish'", []).unwrap();
            conn.execute("UPDATE transforms SET name = 'Tighten', prompt = ?1 WHERE name = 'Shorten'", [RETIRED_PROMPTS[3].1]).unwrap();
            conn.pragma_update(None, "user_version", version).unwrap();
            drop(conn);
            let actions = Db::open(&path).unwrap().list_transforms().unwrap();
            let prompt = |name: &str| actions.iter().find(|t| t.name == name).unwrap().prompt.clone();
            assert_eq!(prompt("Polish"), "My polish", "v{version}: an edited prompt stays");
            assert_eq!(prompt("Tighten"), BUILTIN_TRANSFORMS[3].1, "v{version}: a renamed built-in still updates");
            let conn = Connection::open(&path).unwrap();
            conn.execute("UPDATE transforms SET name = 'Shorten' WHERE name = 'Tighten'", []).unwrap();
            conn.execute("UPDATE transforms SET prompt = ?1 WHERE name = 'Polish'", [BUILTIN_TRANSFORMS[0].1]).unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn db_upsert_rejects_unknown_output(db: &Db) -> bool {
        let t = Transform { id: None, name: "x".into(), prompt: "y".into(), builtin: false, output: "email".into() };
        db.upsert_transform(t).is_err()
    }

    #[test]
    fn failed_migration_rolls_back_and_partial_schema_recovers() {
        let dir = std::env::temp_dir().join(format!("opstrellis-migration-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE transforms (id INTEGER PRIMARY KEY, name TEXT NOT NULL, prompt TEXT NOT NULL, builtin INTEGER NOT NULL DEFAULT 0);
             INSERT INTO transforms (name, prompt) VALUES ('Mine', 'keep');
             CREATE TRIGGER fail_seed BEFORE INSERT ON transforms BEGIN SELECT RAISE(FAIL, 'disk failure'); END;"
        ).unwrap();
        assert!(Db::open(&path).is_err());
        let has_output: i64 = conn.query_row("SELECT COUNT(*) FROM pragma_table_info('transforms') WHERE name = 'output'", [], |r| r.get(0)).unwrap();
        let has_kind: i64 = conn.query_row("SELECT COUNT(*) FROM pragma_table_info('notes') WHERE name = 'kind'", [], |r| r.get(0)).unwrap();
        assert_eq!((has_output, has_kind), (0, 0), "no half-migrated schema on failure");
        // Also recover a database interrupted by the original, nontransactional migration.
        conn.execute_batch("DROP TRIGGER fail_seed; ALTER TABLE transforms ADD COLUMN output TEXT NOT NULL DEFAULT 'replace';").unwrap();
        let db = Db::open(&path).unwrap();
        assert!(db.list_notes(Some("meeting")).unwrap().is_empty());
        assert_eq!(db.list_transforms().unwrap().len(), 3);
        drop(db);
        // A crash after both ALTERs, but before seeding, must also recover on the next open.
        conn.execute_batch("DELETE FROM transforms WHERE output = 'copy'; PRAGMA user_version = 0;").unwrap();
        let db = Db::open(&path).unwrap();
        assert_eq!(db.list_transforms().unwrap().len(), 3);
        let mut action = db.list_transforms().unwrap().into_iter().find(|t| t.name == "Reply").unwrap();
        action.name = "My renamed reply".into();
        db.upsert_transform(action).unwrap();
        drop(db);
        assert_eq!(Db::open(&path).unwrap().list_transforms().unwrap().len(), 3, "completed migrations do not reseed renamed actions");
        drop(conn);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn repolish_preserves_concurrent_edits_and_deletions() {
        let dir = std::env::temp_dir().join(format!("opstrellis-repolish-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("db")).unwrap();
        let _ = db.add_history(&HistoryItem { raw_text: "raw".into(), text: "original".into(), status: "ok".into(), ..Default::default() }, true).unwrap();
        let item = db.list_history(None, 1, 0).unwrap().pop().unwrap();
        db.update_history_text(item.id, "user correction").unwrap();
        assert!(!db.repolish_history_text(&item, "stale AI answer").unwrap());
        let current = db.get_history(item.id).unwrap().unwrap();
        assert_eq!(current.text, "user correction");
        assert!(db.repolish_history_text(&current, "polished text").unwrap());
        assert_eq!(db.get_history(item.id).unwrap().unwrap().words, 2);
        db.delete_history(item.id).unwrap();
        assert!(!db.repolish_history_text(&current, "resurrected").unwrap());
        assert!(db.get_history(item.id).unwrap().is_none());
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn history_privacy_and_aggregates() {
        let dir = std::env::temp_dir().join(format!("opstrellis-db-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db")).unwrap();
        let item = HistoryItem {
            created_at: now_rfc3339(),
            raw_text: "um hello".into(),
            text: "Hello".into(),
            category: "other".into(),
            audio_ms: 30_000,
            words: 1,
            status: "ok".into(),
            mode: "dictate".into(),
            ..Default::default()
        };
        let _ = db.add_history(&item, false).unwrap();
        assert!(db.list_history(None, 10, 0).unwrap().is_empty(), "privacy mode stores nothing");
        let _ = db.add_history(&item, true).unwrap();
        assert_eq!(db.list_history(Some("hel"), 10, 0).unwrap().len(), 1);
        let _ = db.add_history(&HistoryItem { app_name: Some("Slack".into()), category: "work_messages".into(), ..item.clone() }, true).unwrap();
        let apps = db.list_apps(10).unwrap();
        assert_eq!((apps.len(), apps[0].app_name.as_str(), apps[0].category.as_str()), (1, "Slack", "work_messages"));
        // The latest classification is reported, whatever came before.
        let _ = db.add_history(&HistoryItem { app_name: Some("Slack".into()), category: "personal_messages".into(), ..item.clone() }, true).unwrap();
        let _ = db.add_history(&HistoryItem { app_name: Some("Firefox".into()), category: "emails".into(), ..item.clone() }, true).unwrap();
        let apps = db.list_apps(10).unwrap();
        assert_eq!(apps.iter().map(|a| (a.app_name.as_str(), a.category.as_str(), a.count)).collect::<Vec<_>>(),
                   vec![("Slack", "personal_messages", 2), ("Firefox", "emails", 1)]);
        assert_eq!(db.stats(Local::now().date_naive()).unwrap().total_dictations, 5);
        db.clear_history().unwrap();
        assert_eq!(db.stats(Local::now().date_naive()).unwrap().total_words, 5, "aggregates survive deletion");
        assert_eq!(db.list_transforms().unwrap().len(), BUILTIN_TRANSFORMS.len() + BUILTIN_COPY_ACTIONS.len());

        // Auto-learn never overrides a hand-made entry, and updates its own earlier guesses.
        use crate::learn::Correction;
        db.upsert_dictionary(DictEntry { id: None, phrase: "nest dev".into(), replacement: Some("feature/nest_dev".into()), created_at: None, learned: false }).unwrap();
        let learned = db
            .learn(vec![
                Correction { phrase: "nest dev".into(), replacement: Some("nest_dev".into()) },
                Correction { phrase: "Karl".into(), replacement: Some("Carl".into()) },
            ])
            .unwrap();
        assert_eq!(learned.added.len(), 1);
        db.learn(vec![Correction { phrase: "karl".into(), replacement: Some("Karol".into()) }]).unwrap();
        let dict = db.list_dictionary().unwrap();
        assert_eq!(dict.len(), 2);
        assert!(dict.iter().any(|d| d.phrase == "nest dev" && d.replacement.as_deref() == Some("feature/nest_dev") && !d.learned));
        assert!(dict.iter().any(|d| d.replacement.as_deref() == Some("Karol") && d.learned));

        // Correcting a learned output again repoints the original guess instead of chaining it,
        // and correcting it back to the original phrase drops the guess.
        db.learn(vec![Correction { phrase: "Karol".into(), replacement: Some("Carol".into()) }]).unwrap();
        let reps = |db: &Db| db.list_dictionary().unwrap().into_iter().filter(|d| d.learned).map(|d| (d.phrase, d.replacement)).collect::<Vec<_>>();
        assert_eq!(reps(&db), vec![("Karol".into(), Some("Carol".into())), ("karl".into(), Some("Carol".into()))]);
        db.learn(vec![Correction { phrase: "Carol".into(), replacement: Some("Karol".into()) }]).unwrap();
        assert_eq!(reps(&db), vec![("Carol".into(), Some("Karol".into())), ("karl".into(), Some("Karol".into()))]);

        // A new capitalization of a learned spelling updates it.
        db.learn(vec![Correction { phrase: "Github".into(), replacement: None }]).unwrap();
        db.learn(vec![Correction { phrase: "GitHub".into(), replacement: None }]).unwrap();
        assert!(reps(&db).contains(&("GitHub".into(), None)) && !reps(&db).contains(&("Github".into(), None)));

        // Manual entries win regardless of Unicode case or hyphenation.
        db.upsert_dictionary(DictEntry { id: None, phrase: "Émilie".into(), replacement: None, created_at: None, learned: false }).unwrap();
        db.upsert_dictionary(DictEntry { id: None, phrase: "co pilot".into(), replacement: Some("Copilot".into()), created_at: None, learned: false }).unwrap();
        assert!(db.learn(vec![Correction { phrase: "émilie".into(), replacement: Some("Emily".into()) }, Correction { phrase: "co-pilot".into(), replacement: Some("copilot".into()) }]).unwrap().added.is_empty());

        // A correction that doesn't sound like a misrecognition waits for a repeat.
        let swap = || vec![Correction { phrase: "Bob".into(), replacement: Some("Alice".into()) }];
        let first = db.learn(swap()).unwrap();
        assert!(first.added.is_empty() && first.pending.len() == 1);
        let second = db.learn(swap()).unwrap();
        assert!(second.added.len() == 1 && second.pending.is_empty());

        // Hints: most used dictionary entries first, then the built-in terms the user says.
        let alice = second.added[0].id.unwrap();
        db.record_usage(&[alice, alice], &["kubectl", "PostgreSQL", "kubectl"]).unwrap();
        let v = db.vocabulary(100, 10, |_| true, &[]).unwrap();
        assert_eq!(v[0], "Alice");
        assert!(v.ends_with(&["kubectl".into(), "PostgreSQL".into()]), "{v:?}");
        assert_eq!(db.vocabulary(2, 10, |_| true, &[]).unwrap().len(), 2);

        // Then the terms of enabled packs in pack order, so they help before they are ever said.
        let dict = db.vocabulary(100, 0, |_| true, &[]).unwrap().len();
        let packs: Vec<String> = ["EBITDA", "kubectl", "GAAP", "IFRS"].map(String::from).into();
        let v = db.vocabulary(100, 3, |t| t != "PostgreSQL", &packs).unwrap();
        assert_eq!(&v[dict..], ["kubectl", "EBITDA", "GAAP"], "a term of a pack turned off is left out; a repeat takes no slot");
        assert_eq!(&db.vocabulary(dict + 2, 30, |_| true, &packs).unwrap()[dict..], ["kubectl", "PostgreSQL"], "the total limit holds");
    }
}
