//! Back up, restore and import the library (dictionary, snippets, actions, notes, style settings).
//! A backup file may come from someone else, so everything in it is validated before anything is
//! written, and the import itself is one SQLite transaction.
use crate::context::CATEGORIES;
use crate::db::{self, plain_line, plain_text, Db, DICT_MAX_BYTES, DICT_MAX_ENTRIES};
use crate::llm::{NOTES_MAX, OUTPUT_LANGUAGES};
use crate::settings::Settings;
use crate::App;
use anyhow::{anyhow, bail, ensure, Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use tauri::Emitter;

const VERSION: u64 = 1;
const MAX_BYTES: usize = 5 * 1024 * 1024;
const MAX_ENTRIES: usize = 5000;
/// Rules that make an app "AI prompts" can turn a plain shell into a rewrite target; they are never imported.
const NO_IMPORT_CATEGORY: &str = "ai_prompts";
const MAX_PHRASE: usize = 100;
const MAX_REPLACEMENT: usize = 200;
const MAX_TRIGGER: usize = 100;
const MAX_EXPANSION: usize = 5000;
const MAX_NAME: usize = 60;
const MAX_PROMPT: usize = 2000;
const MAX_TITLE: usize = 200;
const MAX_BODY: usize = 200_000;
const MAX_RULES: usize = 500;
const MAX_RULE_KEY: usize = 100;
const STYLE_CATEGORIES: &[&str] = &["personal", "work", "email", "ai", "other"];
const TONES: &[&str] = &["formal", "casual", "very_casual", "excited"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DictItem {
    pub(crate) phrase: String,
    #[serde(default)]
    pub(crate) replacement: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SnippetItem {
    pub(crate) trigger: String,
    pub(crate) expansion: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActionItem {
    pub(crate) name: String,
    pub(crate) prompt: String,
    #[serde(default = "replace")]
    pub(crate) output: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NoteItem {
    title: String,
    body: String,
    #[serde(default = "note")]
    kind: String,
    #[serde(default)]
    updated_at: Option<String>,
}

fn replace() -> String {
    "replace".into()
}

fn note() -> String {
    "note".into()
}

#[derive(Debug, Default, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CleanupPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    remove_fillers: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    voice_commands: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    auto_learn: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tech_terms: Option<bool>,
    /// Built-in term pack ids only; imported packs are specific to a machine.
    #[serde(skip_serializing_if = "Option::is_none")]
    term_packs: Option<Vec<String>>,
}

/// The settings a backup carries, already sanitised. Only what is present is applied.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    cleanup: CleanupPatch,
    styles: BTreeMap<String, String>,
    style_notes: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_style: Option<String>,
    app_categories: BTreeMap<String, String>,
    /// style category -> action name (ids differ between installs)
    auto_actions: BTreeMap<String, String>,
    /// style category -> language code AI polish writes in
    output_languages: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    review_rewrites: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Export<'a> {
    version: u64,
    app_version: &'a str,
    exported_at: String,
    dictionary: Vec<DictItem>,
    snippets: Vec<SnippetItem>,
    actions: Vec<ActionItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<Vec<NoteItem>>,
    settings: SettingsPatch,
}

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    dictionary: Vec<DictItem>,
    #[serde(default)]
    snippets: Vec<SnippetItem>,
    #[serde(default)]
    actions: Vec<ActionItem>,
    #[serde(default)]
    notes: Vec<NoteItem>,
    #[serde(default)]
    settings: Option<Value>,
}

#[derive(Debug)]
struct Parsed {
    dictionary: Vec<DictItem>,
    snippets: Vec<SnippetItem>,
    actions: Vec<ActionItem>,
    notes: Vec<NoteItem>,
    settings: Option<SettingsPatch>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct AddSkip {
    pub add: usize,
    pub skip: usize,
}

/// What restoring the settings changes: the values are replaced, not merged with what is set now.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDetail {
    pub language: Option<String>,
    pub prompt_style: Option<String>,
    /// How many cleanup switches the file sets.
    pub cleanup: usize,
    /// style category -> tone
    pub styles: BTreeMap<String, String>,
    pub style_notes: usize,
    pub app_rules: usize,
    /// style category -> the action that runs after every dictation; only actions that exist here.
    pub auto_actions: BTreeMap<String, String>,
    /// The built-in term packs to have on (an imported pack stays as it is).
    pub term_packs: Option<Vec<String>>,
    /// style category -> language code
    pub output_languages: BTreeMap<String, String>,
    pub review_rewrites: Option<String>,
}

impl SettingsDetail {
    fn is_empty(&self) -> bool {
        *self == SettingsDetail::default()
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub dictionary: AddSkip,
    pub snippets: AddSkip,
    pub actions: AddSkip,
    pub notes: usize,
    pub has_settings: bool,
    pub settings: Option<SettingsDetail>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub path: String,
    pub dictionary: usize,
    pub snippets: usize,
    pub actions: usize,
    pub notes: usize,
    /// Entries left out because import would refuse them (a field over a length limit or with a control character).
    pub skipped: usize,
}

/// How entries are compared when merging: trimmed, case-insensitive.
fn key(s: &str) -> String {
    s.trim().to_lowercase()
}

fn required(s: &str, what: &str) -> Result<(), String> {
    if s.trim().is_empty() {
        return Err(format!("{what} is empty"));
    }
    Ok(())
}

fn within(s: &str, max: usize, what: &str) -> Result<(), String> {
    if s.chars().count() > max {
        return Err(format!("{what} is longer than {max} characters"));
    }
    Ok(())
}

fn line(s: &str, what: &str) -> Result<(), String> {
    if !plain_line(s) {
        return Err(format!("{what} contains a control character"));
    }
    Ok(())
}

fn text(s: &str, what: &str) -> Result<(), String> {
    if !plain_text(s) {
        return Err(format!("{what} contains a control character other than a newline or tab"));
    }
    Ok(())
}

// The checks below mirror what `Db::upsert_*` enforce (trimming, required fields, output values).

pub(crate) fn dict_item(i: DictItem) -> Result<DictItem, String> {
    let phrase = i.phrase.trim().to_string();
    let replacement = i.replacement.map(|r| r.trim().to_string()).filter(|r| !r.is_empty());
    required(&phrase, "phrase")?;
    within(&phrase, MAX_PHRASE, "phrase")?;
    line(&phrase, "phrase")?;
    if let Some(r) = &replacement {
        within(r, MAX_REPLACEMENT, "replacement")?;
        line(r, "replacement")?;
    }
    Ok(DictItem { phrase, replacement })
}

pub(crate) fn snippet_item(i: SnippetItem) -> Result<SnippetItem, String> {
    let trigger = i.trigger.trim().to_string();
    required(&trigger, "trigger")?;
    required(&i.expansion, "expansion")?;
    within(&trigger, MAX_TRIGGER, "trigger")?;
    within(&i.expansion, MAX_EXPANSION, "expansion")?;
    line(&trigger, "trigger")?;
    text(&i.expansion, "expansion")?;
    Ok(SnippetItem { trigger, expansion: i.expansion })
}

pub(crate) fn action_item(i: ActionItem) -> Result<ActionItem, String> {
    let name = i.name.trim().to_string();
    required(&name, "name")?;
    required(&i.prompt, "prompt")?;
    within(&name, MAX_NAME, "name")?;
    within(&i.prompt, MAX_PROMPT, "prompt")?;
    line(&name, "name")?;
    text(&i.prompt, "prompt")?;
    if !matches!(i.output.as_str(), "replace" | "copy") {
        return Err("output must be \"replace\" or \"copy\"".into());
    }
    Ok(ActionItem { name, prompt: i.prompt, output: i.output })
}

fn note_item(i: NoteItem) -> Result<NoteItem, String> {
    within(&i.title, MAX_TITLE, "title")?;
    within(&i.body, MAX_BODY, "body")?;
    line(&i.title, "title")?;
    text(&i.body, "body")?;
    if !matches!(i.kind.as_str(), "note" | "meeting") {
        return Err("kind must be \"note\" or \"meeting\"".into());
    }
    Ok(i)
}

fn valid_language(l: &str) -> bool {
    l == "auto" || ((2..=3).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_lowercase()))
}

fn string_pairs(v: Option<&Value>) -> Vec<(&str, &str)> {
    v.and_then(Value::as_object).map(|o| o.iter().filter_map(|(k, v)| Some((k.as_str(), v.as_str()?))).collect()).unwrap_or_default()
}

/// Keeps only what OpsTrellis understands; anything else is dropped without complaint.
fn sanitize_settings(v: &Value) -> SettingsPatch {
    let mut p = SettingsPatch::default();
    let Some(o) = v.as_object() else { return p };
    p.language = o.get("language").and_then(Value::as_str).filter(|l| valid_language(l)).map(String::from);
    if let Some(c) = o.get("cleanup").and_then(Value::as_object) {
        let flag = |k: &str| c.get(k).and_then(Value::as_bool);
        p.cleanup = CleanupPatch {
            remove_fillers: flag("removeFillers"),
            voice_commands: flag("voiceCommands"),
            auto_learn: flag("autoLearn"),
            tech_terms: flag("techTerms"),
            term_packs: c.get("termPacks").and_then(Value::as_array).map(|ids| {
                let known = builtin_pack_ids();
                let mut keep: Vec<String> = Vec::new();
                for id in ids.iter().filter_map(Value::as_str).filter(|id| known.iter().any(|k| k == id)) {
                    if !keep.iter().any(|k| k == id) {
                        keep.push(id.into());
                    }
                }
                keep
            }),
        };
    }
    for (cat, tone) in string_pairs(o.get("styles")) {
        if STYLE_CATEGORIES.contains(&cat) && TONES.contains(&tone) {
            p.styles.insert(cat.into(), tone.into());
        }
    }
    for (cat, text) in string_pairs(o.get("styleNotes")) {
        let text = text.trim();
        if STYLE_CATEGORIES.contains(&cat) && !text.is_empty() && text.chars().count() <= NOTES_MAX {
            p.style_notes.insert(cat.into(), text.into());
        }
    }
    p.prompt_style = o.get("promptStyle").and_then(Value::as_str).filter(|s| matches!(*s, "clean" | "structured")).map(String::from);
    for (app, category) in string_pairs(o.get("appCategories")) {
        let app = key(app);
        if !app.is_empty() && app.chars().count() <= MAX_RULE_KEY && CATEGORIES.contains(&category) {
            p.app_categories.insert(app, category.into());
        }
    }
    while p.app_categories.len() > MAX_RULES {
        p.app_categories.pop_last();
    }
    for (cat, code) in string_pairs(o.get("outputLanguages")) {
        if STYLE_CATEGORIES.contains(&cat) && OUTPUT_LANGUAGES.iter().any(|(c, _)| *c == code) {
            p.output_languages.insert(cat.into(), code.into());
        }
    }
    p.review_rewrites = o.get("reviewRewrites").and_then(Value::as_str).filter(|v| matches!(*v, "long" | "always" | "never")).map(String::from);
    for (cat, name) in string_pairs(o.get("autoActions")) {
        let name = name.trim();
        if STYLE_CATEGORIES.contains(&cat) && !name.is_empty() && name.chars().count() <= MAX_NAME {
            p.auto_actions.insert(cat.into(), name.into());
        }
    }
    p
}

fn import_settings(v: &Value) -> SettingsPatch {
    let mut p = sanitize_settings(v);
    p.app_categories.retain(|_, category| category != NO_IMPORT_CATEGORY);
    p
}

/// What applying `p` changes. Auto actions count only if an action of that name exists already.
fn detail(p: &SettingsPatch, action_ids: &HashMap<String, i64>) -> SettingsDetail {
    let c = &p.cleanup;
    SettingsDetail {
        language: p.language.clone(),
        prompt_style: p.prompt_style.clone(),
        cleanup: [c.remove_fillers, c.voice_commands, c.auto_learn, c.tech_terms].iter().flatten().count(),
        styles: p.styles.clone(),
        style_notes: p.style_notes.len(),
        app_rules: p.app_categories.len(),
        auto_actions: p.auto_actions.iter().filter(|(_, name)| action_ids.contains_key(&key(name))).map(|(c, n)| (c.clone(), n.clone())).collect(),
        term_packs: c.term_packs.clone(),
        output_languages: p.output_languages.clone(),
        review_rewrites: p.review_rewrites.clone(),
    }
}

/// Ids of the term packs that ship with the app (the "tech" pack is its own switch).
fn builtin_pack_ids() -> Vec<String> {
    crate::terms::list(false, &[]).into_iter().filter(|p| p.builtin && p.id != "tech").map(|p| p.id).collect()
}

/// Backed-up values replace the same keys in the current settings; keys the backup lacks stay.
/// An auto action whose name matches no action here is skipped.
fn apply_settings(s: &mut Settings, p: &SettingsPatch, action_ids: &HashMap<String, i64>) {
    if let Some(l) = &p.language {
        s.language = l.clone();
    }
    let c = &p.cleanup;
    s.cleanup.remove_fillers = c.remove_fillers.unwrap_or(s.cleanup.remove_fillers);
    s.cleanup.voice_commands = c.voice_commands.unwrap_or(s.cleanup.voice_commands);
    s.cleanup.auto_learn = c.auto_learn.unwrap_or(s.cleanup.auto_learn);
    s.cleanup.tech_terms = c.tech_terms.unwrap_or(s.cleanup.tech_terms);
    if let Some(ids) = &c.term_packs {
        let builtin = builtin_pack_ids();
        s.cleanup.term_packs.retain(|id| !builtin.contains(id));
        s.cleanup.term_packs.extend(ids.iter().cloned());
    }
    s.styles.extend(p.styles.clone());
    s.style_notes.extend(p.style_notes.clone());
    if let Some(ps) = &p.prompt_style {
        s.prompt_style = ps.clone();
    }
    s.app_categories.extend(p.app_categories.clone());
    s.output_languages.extend(p.output_languages.clone());
    if let Some(r) = &p.review_rewrites {
        s.review_rewrites = r.clone();
    }
    for (category, name) in &p.auto_actions {
        if let Some(id) = action_ids.get(&key(name)) {
            s.auto_transforms.insert(category.clone(), *id);
        }
    }
}

fn action_ids(db: &Db) -> Result<HashMap<String, i64>> {
    let mut ids = HashMap::new();
    for t in db.list_transforms()? {
        if let Some(id) = t.id {
            ids.entry(key(&t.name)).or_insert(id);
        }
    }
    Ok(ids)
}

fn parse(json: &str) -> Result<Parsed> {
    ensure!(json.len() <= MAX_BYTES, "This file is over 5 MB, too big to be an OpsTrellis backup.");
    let v: Value = serde_json::from_str(json).map_err(|_| anyhow!("This isn't an OpsTrellis backup (the file isn't valid JSON)."))?;
    match v.get("version").and_then(Value::as_u64) {
        Some(VERSION) => {}
        Some(n) => bail!("This backup uses format version {n}, which this version of OpsTrellis can't read (it reads version {VERSION}). Update OpsTrellis and try again."),
        None => bail!("This isn't an OpsTrellis backup (no version number)."),
    }
    let raw: Raw = serde_json::from_value(v).map_err(|e| anyhow!("This isn't a valid OpsTrellis backup: {e}"))?;
    for (n, len, max) in [
        ("dictionary entries", raw.dictionary.len(), DICT_MAX_ENTRIES),
        ("snippets", raw.snippets.len(), MAX_ENTRIES),
        ("actions", raw.actions.len(), MAX_ENTRIES),
        ("notes", raw.notes.len(), MAX_ENTRIES),
    ] {
        ensure!(len <= max, "The backup has {len} {n}; the limit is {max}.");
    }
    Ok(Parsed {
        dictionary: raw.dictionary.into_iter().enumerate().map(|(i, e)| dict_item(e).map_err(|m| anyhow!("Dictionary entry {}: {m}.", i + 1))).collect::<Result<_>>()?,
        snippets: raw.snippets.into_iter().enumerate().map(|(i, e)| snippet_item(e).map_err(|m| anyhow!("Snippet {}: {m}.", i + 1))).collect::<Result<_>>()?,
        actions: raw.actions.into_iter().enumerate().map(|(i, e)| action_item(e).map_err(|m| anyhow!("Action {}: {m}.", i + 1))).collect::<Result<_>>()?,
        notes: raw.notes.into_iter().enumerate().map(|(i, e)| note_item(e).map_err(|m| anyhow!("Note {}: {m}.", i + 1))).collect::<Result<_>>()?,
        settings: raw.settings.as_ref().map(import_settings),
    })
}

fn existing_keys(c: &Connection, sql: &str) -> Result<HashSet<String>> {
    let mut stmt = c.prepare(sql)?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.map(|r| r.map(|k| key(&k)).map_err(Into::into)).collect()
}

fn merge_dictionary(c: &Connection, items: &[DictItem], write: bool) -> Result<AddSkip> {
    // Only the user's own entries count: an imported one then wins over a team entry, as one typed in does.
    let mut seen = existing_keys(c, "SELECT phrase FROM dictionary WHERE source != 'team'")?;
    let mut fresh = Vec::new();
    for it in items {
        if seen.insert(key(&it.phrase)) {
            fresh.push(it);
        }
    }
    // The whole dictionary becomes one regex: refuse what would make it too big to build.
    let (have, bytes) = db::dictionary_usage(c, None)?;
    let size = |it: &DictItem| it.phrase.len() + it.replacement.as_ref().map_or(0, String::len);
    let (mut entries, mut total, mut fit) = (have, bytes, 0);
    for it in &fresh {
        entries += 1;
        total += size(it);
        if entries > DICT_MAX_ENTRIES || total > DICT_MAX_BYTES {
            break;
        }
        fit += 1;
    }
    ensure!(
        fit == fresh.len(),
        "The dictionary can hold {DICT_MAX_ENTRIES} entries and {DICT_MAX_BYTES} bytes of words and replacements. It has {have} entries now, so {fit} of the {} new ones fit. Nothing was imported.",
        fresh.len()
    );
    if write {
        let now = db::now_rfc3339();
        for it in &fresh {
            c.execute("INSERT INTO dictionary (phrase, replacement, created_at) VALUES (?1, ?2, ?3)", params![it.phrase, it.replacement, now])?;
        }
    }
    Ok(AddSkip { add: fresh.len(), skip: items.len() - fresh.len() })
}

fn merge_snippets(c: &Connection, items: &[SnippetItem], write: bool) -> Result<AddSkip> {
    let mut seen = existing_keys(c, "SELECT trigger FROM snippets WHERE team_id IS NULL")?;
    let (now, mut out) = (db::now_rfc3339(), AddSkip::default());
    for it in items {
        if !seen.insert(key(&it.trigger)) {
            out.skip += 1;
            continue;
        }
        if write {
            c.execute("INSERT INTO snippets (trigger, expansion, created_at) VALUES (?1, ?2, ?3)", params![it.trigger, it.expansion, now])?;
        }
        out.add += 1;
    }
    Ok(out)
}

/// Built-in actions are in `transforms` too, so an imported action named like one is skipped.
fn merge_actions(c: &Connection, items: &[ActionItem], write: bool) -> Result<AddSkip> {
    let mut seen = existing_keys(c, "SELECT name FROM transforms WHERE team_id IS NULL")?;
    let mut out = AddSkip::default();
    for it in items {
        if !seen.insert(key(&it.name)) {
            out.skip += 1;
            continue;
        }
        if write {
            c.execute("INSERT INTO transforms (name, prompt, builtin, output) VALUES (?1, ?2, 0, ?3)", params![it.name, it.prompt, it.output])?;
        }
        out.add += 1;
    }
    Ok(out)
}

/// Returns how many notes were (or would be) added: those with the same title and body as an existing one are not.
fn merge_notes(c: &Connection, items: &[NoteItem], write: bool) -> Result<usize> {
    let mut seen: HashSet<(String, String)> = {
        let mut stmt = c.prepare("SELECT title, body FROM notes")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    let (now, mut added) = (db::now_rfc3339(), 0);
    for it in items {
        if !seen.insert((it.title.clone(), it.body.clone())) {
            continue;
        }
        if write {
            let at = it.updated_at.as_deref().filter(|t| chrono::DateTime::parse_from_rfc3339(t).is_ok()).unwrap_or(&now);
            c.execute("INSERT INTO notes (title, body, updated_at, kind) VALUES (?1, ?2, ?3, ?4)", params![it.title, it.body, at, it.kind])?;
        }
        added += 1;
    }
    Ok(added)
}

/// One transaction: everything is added, or (on any error) nothing. `write: false` only counts.
/// `action_ids` are the actions that exist before the import.
fn run(db: &Db, p: &Parsed, action_ids: &HashMap<String, i64>, notes: bool, write: bool) -> Result<Summary> {
    let settings = p.settings.as_ref().map(|s| detail(s, action_ids)).filter(|d| !d.is_empty());
    db.in_transaction(|tx| {
        Ok(Summary {
            dictionary: merge_dictionary(tx, &p.dictionary, write)?,
            snippets: merge_snippets(tx, &p.snippets, write)?,
            actions: merge_actions(tx, &p.actions, write)?,
            notes: if notes { merge_notes(tx, &p.notes, write)? } else { 0 },
            has_settings: settings.is_some(),
            settings,
        })
    })
}

pub fn preview(db: &Db, json: &str) -> Result<Summary> {
    run(db, &parse(json)?, &action_ids(db)?, true, false)
}

/// Adds the library and returns the settings to apply with the action ids as they were before the
/// import: a file can't make one of its own new actions run after every dictation.
fn import_library(db: &Db, json: &str, settings: bool, notes: bool) -> Result<(Summary, Option<(SettingsPatch, HashMap<String, i64>)>)> {
    let parsed = parse(json)?;
    let ids = action_ids(db)?;
    let mut summary = run(db, &parsed, &ids, notes, true)?;
    if !settings || summary.settings.is_none() {
        (summary.has_settings, summary.settings) = (false, None);
        return Ok((summary, None));
    }
    Ok((summary, parsed.settings.map(|p| (p, ids))))
}

pub fn import(app: &App, json: &str, settings: bool, notes: bool) -> Result<Summary> {
    let (summary, restore) = import_library(&app.db, json, settings, notes)?;
    if let Some((patch, ids)) = restore {
        let next = app.update_settings(|s| apply_settings(s, &patch, &ids)).context("The library was imported, but the settings couldn't be saved")?;
        let _ = app.handle.emit("settings-changed", next);
    }
    Ok(summary)
}

pub fn import_dictionary_text(db: &Db, text: &str) -> Result<AddSkip> {
    let items = parse_dictionary_text(text)?;
    db.in_transaction(|tx| merge_dictionary(tx, &items, true))
}

/// `phrase` or `phrase<sep>replacement`; the first separator found wins, in this order.
fn split_line(line: &str) -> (&str, Option<&str>) {
    for sep in ["\t", " => ", " -> ", " \u{2192} ", ","] {
        if let Some((phrase, replacement)) = line.split_once(sep) {
            return (phrase.trim(), Some(replacement.trim()));
        }
    }
    (line, None)
}

fn parse_dictionary_text(text: &str) -> Result<Vec<DictItem>> {
    ensure!(text.len() <= MAX_BYTES, "This file is over 5 MB, too big for a word list.");
    let mut out = Vec::new();
    for (n, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        // Only the start is trimmed: a trailing separator means "no replacement".
        let line = line.trim_start();
        if line.trim_end().is_empty() || line.starts_with('#') {
            continue;
        }
        ensure!(out.len() < DICT_MAX_ENTRIES, "A list can have at most {DICT_MAX_ENTRIES} entries.");
        let (phrase, replacement) = split_line(line);
        let item = DictItem { phrase: phrase.into(), replacement: replacement.map(String::from) };
        out.push(dict_item(item).map_err(|m| anyhow!("Line {}: {m}.", n + 1))?);
    }
    Ok(out)
}

fn kept<T>(r: Result<T, String>, skipped: &mut usize) -> Option<T> {
    r.map_err(|_| *skipped += 1).ok()
}

fn build(db: &Db, settings: &Settings, include_notes: bool, app_version: &str) -> Result<(String, ExportResult)> {
    let mut skipped = 0;
    // Oldest first, so an import adds them in the order they were made.
    // Team entries come back by syncing, not from a backup. Entries beyond the dictionary budget are
    // left out (a dictionary from before the budget existed), so the file can be imported again.
    let mut dictionary: Vec<DictItem> = db
        .list_dictionary_rows()?
        .into_iter()
        .rev()
        .filter(|r| r.source != "team")
        .filter_map(|r| kept(dict_item(DictItem { phrase: r.entry.phrase, replacement: r.entry.replacement }), &mut skipped))
        .collect();
    let mut bytes = 0;
    let fits = dictionary
        .iter()
        .take(DICT_MAX_ENTRIES)
        .take_while(|e| {
            bytes += e.phrase.len() + e.replacement.as_ref().map_or(0, String::len);
            bytes <= DICT_MAX_BYTES
        })
        .count();
    skipped += dictionary.len() - fits;
    dictionary.truncate(fits);
    let snippets: Vec<SnippetItem> = db
        .list_snippet_rows()?
        .into_iter()
        .rev()
        .filter(|r| !r.team)
        .filter_map(|r| kept(snippet_item(SnippetItem { trigger: r.entry.trigger, expansion: r.entry.expansion }), &mut skipped))
        .collect();
    let transforms = db.list_transforms()?;
    let names: HashMap<i64, String> = transforms.iter().filter_map(|t| Some((t.id?, t.name.clone()))).collect();
    let actions: Vec<ActionItem> = transforms
        .into_iter()
        .filter(|t| !t.builtin)
        .filter_map(|t| kept(action_item(ActionItem { name: t.name, prompt: t.prompt, output: t.output }), &mut skipped))
        .collect();
    let notes = if include_notes {
        Some(
            db.list_notes(None)?
                .into_iter()
                .rev()
                .filter_map(|n| kept(note_item(NoteItem { title: n.title, body: n.body, kind: n.kind, updated_at: n.updated_at }), &mut skipped))
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    let auto_actions: BTreeMap<&String, &String> = settings.auto_transforms.iter().filter_map(|(c, id)| Some((c, names.get(id)?))).collect();
    let settings = sanitize_settings(&json!({
        "language": settings.language,
        "cleanup": settings.cleanup,
        "styles": settings.styles,
        "styleNotes": settings.style_notes,
        "promptStyle": settings.prompt_style,
        "appCategories": settings.app_categories,
        "outputLanguages": settings.output_languages,
        "reviewRewrites": settings.review_rewrites,
        "autoActions": auto_actions,
    }));
    let result = ExportResult {
        dictionary: dictionary.len(),
        snippets: snippets.len(),
        actions: actions.len(),
        notes: notes.as_ref().map_or(0, Vec::len),
        skipped,
        ..Default::default()
    };
    let backup = Export { version: VERSION, app_version, exported_at: db::now_rfc3339(), dictionary, snippets, actions, notes, settings };
    let json = serde_json::to_string_pretty(&backup)?;
    ensure!(
        json.len() <= MAX_BYTES,
        "The backup would be over 5 MB, more than OpsTrellis can import again.{}",
        if include_notes { " Export without notes and meetings." } else { "" }
    );
    Ok((json, result))
}

/// Writes the backup to the Downloads folder and shows it there.
pub fn export(app: &App, include_notes: bool) -> Result<ExportResult> {
    let version = app.handle.package_info().version.to_string();
    let (json, mut result) = build(&app.db, &app.settings(), include_notes, &version)?;
    let dir = dirs::download_dir().or_else(dirs::home_dir).context("Couldn't find your Downloads folder")?;
    let path = dir.join(format!("opstrellis-backup-{}.json", chrono::Local::now().format("%Y%m%d-%H%M%S")));
    std::fs::write(&path, json).with_context(|| format!("Couldn't write {}", path.display()))?;
    let _ = tauri_plugin_opener::reveal_item_in_dir(&path);
    result.path = path.display().to_string();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> Db {
        let dir = std::env::temp_dir().join(format!("opstrellis-backup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(&dir.join("db")).unwrap()
    }

    fn file(body: Value) -> String {
        let mut v = json!({ "version": 1, "appVersion": "0.0.0", "exportedAt": "2026-01-01T00:00:00Z" });
        v.as_object_mut().unwrap().extend(body.as_object().unwrap().clone());
        v.to_string()
    }

    fn import_lib(db: &Db, json: &str, notes: bool) -> Result<Summary> {
        run(db, &parse(json)?, &action_ids(db)?, notes, true)
    }

    fn add_action(db: &Db, name: &str, output: &str) -> i64 {
        db.upsert_transform(db::Transform { id: None, name: name.into(), prompt: "Do it.".into(), builtin: false, output: output.into() }).unwrap().id.unwrap()
    }

    fn add_note(db: &Db, title: &str, body: &str, kind: &str) {
        db.upsert_note(db::Note { id: None, title: title.into(), body: body.into(), updated_at: None, kind: kind.into() }).unwrap();
    }

    fn rejected(json: Value) -> String {
        format!("{:#}", parse(&file(json)).unwrap_err())
    }

    #[test]
    fn round_trip_into_an_empty_library() {
        let src = temp_db();
        for (p, r) in [("Kubernetes", None), ("my email", Some("alex@example.com"))] {
            src.upsert_dictionary(db::DictEntry { id: None, phrase: p.into(), replacement: r.map(String::from), created_at: None, learned: false }).unwrap();
        }
        src.upsert_snippet(db::Snippet { id: None, trigger: "addr".into(), expansion: "221B Baker Street".into(), created_at: None }).unwrap();
        let pirate = add_action(&src, "Pirate", "copy");
        add_note(&src, "Outline", "Intro\nDemo", "note");
        add_note(&src, "Meeting · Oct 14", "## Summary", "meeting");
        let mut s = Settings::default();
        s.language = "de".into();
        s.cleanup.auto_learn = true;
        s.styles.insert("work".into(), "excited".into());
        s.style_notes.insert("email".into(), "Sign off with Best".into());
        s.prompt_style = "structured".into();
        s.app_categories.insert("my-crm".into(), "emails".into());
        s.auto_transforms.insert("work".into(), pirate);
        s.auto_transforms.insert("email".into(), 9999);

        let (json, result) = build(&src, &s, true, "9.9.9").unwrap();
        assert_eq!((result.dictionary, result.snippets, result.actions, result.notes, result.skipped), (2, 1, 1, 2, 0));
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!((v["version"].clone(), v["appVersion"].clone()), (json!(1), json!("9.9.9")));
        assert_eq!(v["settings"]["autoActions"], json!({ "work": "Pirate" }));
        assert!(!json.contains("sttChain") && !json.contains("hotkeys") && !json.contains("baseUrl"));
        assert_eq!(v["actions"].as_array().unwrap().len(), 1, "built-in actions are never exported");

        let dst = temp_db();
        add_action(&dst, "Other", "replace");
        let parsed = parse(&json).unwrap();
        let ids_before = action_ids(&dst).unwrap();
        let summary = run(&dst, &parsed, &ids_before, true, true).unwrap();
        let shown = summary.settings.clone().unwrap();
        assert_eq!((shown.language.as_deref(), shown.cleanup, shown.app_rules, shown.style_notes), (Some("de"), 4, 1, 1));
        assert_eq!(shown.auto_actions.len(), 0, "Pirate didn't exist here before the import");
        assert_eq!(summary, Summary { dictionary: AddSkip { add: 2, skip: 0 }, snippets: AddSkip { add: 1, skip: 0 }, actions: AddSkip { add: 1, skip: 0 }, notes: 2, has_settings: true, settings: Some(shown) });
        let phrases: Vec<_> = dst.list_dictionary().unwrap().into_iter().map(|e| (e.phrase, e.replacement)).collect();
        assert_eq!(phrases, vec![("my email".to_string(), Some("alex@example.com".to_string())), ("Kubernetes".to_string(), None)]);
        assert_eq!(dst.list_snippets().unwrap()[0].expansion, "221B Baker Street");
        let imported = dst.list_transforms().unwrap().into_iter().find(|t| t.name == "Pirate").unwrap();
        assert_eq!((imported.output.as_str(), imported.builtin), ("copy", false));
        assert_ne!(imported.id, Some(pirate), "ids differ between installs");
        assert_eq!(dst.list_notes(Some("meeting")).unwrap().len(), 1);

        let mut restored = Settings::default();
        apply_settings(&mut restored, &parsed.settings.unwrap(), &action_ids(&dst).unwrap());
        assert_eq!((restored.language.as_str(), restored.prompt_style.as_str()), ("de", "structured"));
        assert!(restored.cleanup.auto_learn);
        assert_eq!(restored.styles["work"], "excited");
        assert_eq!(restored.style_notes["email"], "Sign off with Best");
        assert_eq!(restored.app_categories["my-crm"], "emails");
        assert_eq!(restored.auto_transforms.get("work"), Some(&imported.id.unwrap()));
        assert_eq!(restored.auto_transforms.get("email"), None);
        assert_eq!(restored.hotkeys, Settings::default().hotkeys);
    }

    #[test]
    fn export_without_notes_omits_them() {
        let db = temp_db();
        add_note(&db, "Private", "body", "note");
        let (json, result) = build(&db, &Settings::default(), false, "1.0.0").unwrap();
        assert_eq!(result.notes, 0);
        assert!(serde_json::from_str::<Value>(&json).unwrap().get("notes").is_none());
        assert!(!json.contains("Private"));
    }

    #[test]
    fn export_leaves_out_what_import_would_refuse() {
        let db = temp_db();
        add_note(&db, "Long meeting", &"x".repeat(MAX_BODY + 1), "meeting");
        add_note(&db, "Fine", "ok", "note");
        let (json, result) = build(&db, &Settings::default(), true, "1.0.0").unwrap();
        assert_eq!((result.notes, result.skipped), (1, 1));
        assert!(parse(&json).is_ok());
    }

    #[test]
    fn existing_entries_win_case_insensitively() {
        let db = temp_db();
        db.upsert_dictionary(db::DictEntry { id: None, phrase: "Kubernetes".into(), replacement: Some("K8s".into()), created_at: None, learned: false }).unwrap();
        db.upsert_snippet(db::Snippet { id: None, trigger: "Addr".into(), expansion: "mine".into(), created_at: None }).unwrap();
        add_action(&db, "Pirate", "replace");
        add_note(&db, "Outline", "Intro", "note");
        let json = file(json!({
            "dictionary": [
                { "phrase": "  kubernetes ", "replacement": "other" }, { "phrase": "Terraform", "replacement": null },
                { "phrase": "TERRAFORM", "replacement": "dup in file" }
            ],
            "snippets": [{ "trigger": " ADDR", "expansion": "theirs" }, { "trigger": "sig", "expansion": "Best" }],
            "actions": [{ "name": "pirate", "prompt": "x", "output": "copy" }, { "name": "Haiku", "prompt": "x", "output": "replace" }],
            "notes": [
                { "title": "Outline", "body": "Intro", "kind": "note" }, { "title": "Outline", "body": "Changed", "kind": "note" },
                { "title": "Outline", "body": "Changed", "kind": "note" }
            ]
        }));
        let would = preview(&db, &json).unwrap();
        assert_eq!(db.list_dictionary().unwrap().len(), 1, "preview changes nothing");
        let summary = import_lib(&db, &json, true).unwrap();
        assert_eq!(summary.dictionary, AddSkip { add: 1, skip: 2 });
        assert_eq!(summary.snippets, AddSkip { add: 1, skip: 1 });
        assert_eq!(summary.actions, AddSkip { add: 1, skip: 1 });
        assert_eq!(summary.notes, 1);
        assert_eq!(summary, would);
        let kube = db.list_dictionary().unwrap().into_iter().find(|e| e.phrase == "Kubernetes").unwrap();
        assert_eq!(kube.replacement.as_deref(), Some("K8s"));
        assert_eq!(db.list_notes(None).unwrap().len(), 2);
        assert_eq!(import_lib(&db, &json, true).unwrap().dictionary, AddSkip { add: 0, skip: 3 }, "importing twice adds nothing");
    }

    #[test]
    fn notes_are_imported_only_when_asked() {
        let db = temp_db();
        let json = file(json!({ "notes": [{ "title": "A", "body": "b", "kind": "note", "updatedAt": "2025-05-05T10:00:00+00:00" }] }));
        assert_eq!(import_lib(&db, &json, false).unwrap().notes, 0);
        assert!(db.list_notes(None).unwrap().is_empty());
        assert_eq!(preview(&db, &json).unwrap().notes, 1);
        import_lib(&db, &json, true).unwrap();
        assert_eq!(db.list_notes(None).unwrap()[0].updated_at.as_deref(), Some("2025-05-05T10:00:00+00:00"));
    }

    #[test]
    fn built_in_action_names_are_skipped() {
        let db = temp_db();
        let json = file(json!({ "actions": [
            { "name": "Polish", "prompt": "mine", "output": "replace" }, { "name": " summarize ", "prompt": "mine", "output": "replace" }
        ] }));
        assert_eq!(import_lib(&db, &json, false).unwrap().actions, AddSkip { add: 0, skip: 2 });
        assert_eq!(db.list_transforms().unwrap().iter().filter(|t| !t.builtin).count(), 0);
    }

    #[test]
    fn every_limit_is_enforced_at_its_edge() {
        let s = |n: usize| "x".repeat(n);
        let ok = |json: Value| assert!(parse(&file(json)).is_ok());
        ok(json!({ "dictionary": [{ "phrase": s(100), "replacement": s(200) }] }));
        assert!(rejected(json!({ "dictionary": [{ "phrase": s(101) }] })).contains("phrase is longer than 100"));
        assert!(rejected(json!({ "dictionary": [{ "phrase": "a", "replacement": s(201) }] })).contains("replacement is longer than 200"));
        ok(json!({ "snippets": [{ "trigger": s(100), "expansion": s(5000) }] }));
        assert!(rejected(json!({ "snippets": [{ "trigger": s(101), "expansion": "x" }] })).contains("trigger is longer than 100"));
        assert!(rejected(json!({ "snippets": [{ "trigger": "t", "expansion": s(5001) }] })).contains("expansion is longer than 5000"));
        ok(json!({ "actions": [{ "name": s(60), "prompt": s(2000), "output": "copy" }] }));
        assert!(rejected(json!({ "actions": [{ "name": s(61), "prompt": "p", "output": "copy" }] })).contains("name is longer than 60"));
        assert!(rejected(json!({ "actions": [{ "name": "n", "prompt": s(2001), "output": "copy" }] })).contains("prompt is longer than 2000"));
        ok(json!({ "notes": [{ "title": s(200), "body": s(200_000), "kind": "meeting" }] }));
        assert!(rejected(json!({ "notes": [{ "title": s(201), "body": "", "kind": "note" }] })).contains("title is longer than 200"));
        assert!(rejected(json!({ "notes": [{ "title": "t", "body": s(200_001), "kind": "note" }] })).contains("body is longer than 200000"));
    }

    #[test]
    fn empty_fields_and_unknown_values_are_rejected() {
        assert!(rejected(json!({ "dictionary": [{ "phrase": "  " }] })).contains("Dictionary entry 1: phrase is empty"));
        assert!(rejected(json!({ "dictionary": [{ "phrase": "ok" }, { "phrase": "" }] })).contains("Dictionary entry 2"));
        assert!(rejected(json!({ "snippets": [{ "trigger": "", "expansion": "x" }] })).contains("trigger is empty"));
        assert!(rejected(json!({ "snippets": [{ "trigger": "t", "expansion": " " }] })).contains("expansion is empty"));
        assert!(rejected(json!({ "actions": [{ "name": "", "prompt": "p", "output": "copy" }] })).contains("name is empty"));
        assert!(rejected(json!({ "actions": [{ "name": "n", "prompt": "", "output": "copy" }] })).contains("prompt is empty"));
        assert!(rejected(json!({ "actions": [{ "name": "n", "prompt": "p", "output": "paste" }] })).contains("output must be"));
        assert!(rejected(json!({ "notes": [{ "title": "t", "body": "b", "kind": "memo" }] })).contains("kind must be"));
        assert!(rejected(json!({ "dictionary": "nope" })).contains("isn't a valid OpsTrellis backup"));
    }

    #[test]
    fn size_version_and_count_limits() {
        assert!(parse(&"x".repeat(MAX_BYTES + 1)).unwrap_err().to_string().contains("over 5 MB"));
        assert!(parse("{ not json").unwrap_err().to_string().contains("isn't valid JSON"));
        assert!(parse("[]").unwrap_err().to_string().contains("no version"));
        let v2 = json!({ "version": 2 }).to_string();
        assert!(parse(&v2).unwrap_err().to_string().contains("version 2"));
        assert!(parse(&json!({ "version": "1" }).to_string()).is_err());
        let many = |n: usize| json!({ "dictionary": (0..n).map(|i| json!({ "phrase": format!("w{i}") })).collect::<Vec<_>>() });
        assert_eq!(parse(&file(many(DICT_MAX_ENTRIES))).unwrap().dictionary.len(), DICT_MAX_ENTRIES);
        assert!(rejected(many(DICT_MAX_ENTRIES + 1)).contains("2001 dictionary entries; the limit is 2000"));
        let many_snippets = |n: usize| json!({ "snippets": (0..n).map(|i| json!({ "trigger": format!("t{i}"), "expansion": "x" })).collect::<Vec<_>>() });
        assert_eq!(parse(&file(many_snippets(MAX_ENTRIES))).unwrap().snippets.len(), MAX_ENTRIES);
        assert!(rejected(many_snippets(MAX_ENTRIES + 1)).contains("limit is 5000"));
        let many_notes = json!({ "notes": (0..=MAX_ENTRIES).map(|_| json!({ "title": "t", "body": "b" })).collect::<Vec<_>>() });
        assert!(rejected(many_notes).contains("5001 notes"));
    }

    #[test]
    fn an_invalid_file_changes_nothing() {
        let db = temp_db();
        let bad = file(json!({
            "dictionary": [{ "phrase": "Fine" }],
            "snippets": [{ "trigger": "t", "expansion": "x" }],
            "notes": [{ "title": "t", "body": "b", "kind": "memo" }]
        }));
        assert!(import_lib(&db, &bad, true).is_err());
        assert!(db.list_dictionary().unwrap().is_empty() && db.list_snippets().unwrap().is_empty());
    }

    #[test]
    fn a_failure_midway_rolls_everything_back() {
        let db = temp_db();
        db.in_transaction(|tx| Ok(tx.execute_batch("DROP TABLE notes")?)).unwrap();
        let json = file(json!({
            "dictionary": [{ "phrase": "Fine" }],
            "snippets": [{ "trigger": "t", "expansion": "x" }],
            "actions": [{ "name": "New", "prompt": "p" }],
            "notes": [{ "title": "t", "body": "b" }]
        }));
        assert!(import_lib(&db, &json, true).is_err());
        assert!(db.list_dictionary().unwrap().is_empty());
        assert!(db.list_snippets().unwrap().is_empty());
        assert!(db.list_transforms().unwrap().iter().all(|t| t.name != "New"));
        assert_eq!(import_lib(&db, &json, false).unwrap().dictionary.add, 1, "the same file imports once notes are skipped");
    }

    #[test]
    fn settings_are_sanitised() {
        let rules: serde_json::Map<String, Value> = (0..600).map(|i| (format!("app{i:03}"), json!("emails"))).collect();
        let p = sanitize_settings(&json!({
            "language": "fr",
            "cleanup": { "removeFillers": false, "voiceCommands": "yes", "autoLearn": true, "unknown": true },
            "styles": { "work": "formal", "email": "shouting", "weird": "casual", "ai": "very_casual", "personal": 3 },
            "styleNotes": { "work": " Short. ", "email": "x".repeat(NOTES_MAX + 1), "ai": "x".repeat(NOTES_MAX), "bogus": "hi", "other": "  " },
            "promptStyle": "structured",
            "appCategories": Value::Object(rules),
            "autoActions": { "work": " Pirate ", "bogus": "Pirate", "email": "", "ai": "x".repeat(MAX_NAME + 1), "other": 7 },
            "hotkeys": { "dictate": "F1" }, "sttChain": []
        }));
        assert_eq!(p.language.as_deref(), Some("fr"));
        assert_eq!(p.cleanup, CleanupPatch { remove_fillers: Some(false), voice_commands: None, auto_learn: Some(true), tech_terms: None, term_packs: None });
        assert_eq!(p.styles, BTreeMap::from([("work".into(), "formal".into()), ("ai".into(), "very_casual".into())]));
        assert_eq!(p.style_notes.keys().collect::<Vec<_>>(), ["ai", "work"]);
        assert_eq!(p.style_notes["work"], "Short.");
        assert_eq!(p.prompt_style.as_deref(), Some("structured"));
        assert_eq!(p.app_categories.len(), MAX_RULES);
        assert_eq!(p.auto_actions, BTreeMap::from([("work".to_string(), "Pirate".to_string())]));

        let bad = sanitize_settings(&json!({
            "language": "english", "promptStyle": "fancy", "appCategories": { "": "emails", "crm": "nonsense", "x": "emails" },
            "styles": { "work": "formal" }
        }));
        assert_eq!((bad.language, bad.prompt_style), (None, None));
        assert_eq!(bad.app_categories, BTreeMap::from([("x".to_string(), "emails".to_string())]));
        assert_eq!(sanitize_settings(&json!("nope")), SettingsPatch::default());
        assert_eq!(sanitize_settings(&json!({ "hotkeys": {} })), SettingsPatch::default());
        let long_key = sanitize_settings(&json!({ "appCategories": { "a".repeat(101): "emails", "A".repeat(100): "emails" } }));
        assert_eq!(long_key.app_categories.len(), 1);
        assert!(sanitize_settings(&json!({ "language": "auto" })).language.is_some());
    }

    #[test]
    fn auto_actions_resolve_by_name_and_unknown_names_are_dropped() {
        let db = temp_db();
        let id = add_action(&db, "Pirate", "replace");
        let polish = db.list_transforms().unwrap().into_iter().find(|t| t.name == "Polish").unwrap().id.unwrap();
        let patch = sanitize_settings(&json!({ "autoActions": { "work": "pirate", "email": "Polish", "ai": "Nope" } }));
        let mut s = Settings::default();
        s.auto_transforms.insert("ai".into(), 42);
        s.auto_transforms.insert("other".into(), 7);
        apply_settings(&mut s, &patch, &action_ids(&db).unwrap());
        assert_eq!((s.auto_transforms.get("work"), s.auto_transforms.get("email")), (Some(&id), Some(&polish)));
        assert_eq!((s.auto_transforms.get("ai"), s.auto_transforms.get("other")), (Some(&42), Some(&7)), "unresolved names leave existing rules alone");
    }

    #[test]
    fn restored_settings_keep_keys_the_backup_lacks() {
        let mut s = Settings::default();
        s.styles.insert("email".into(), "casual".into());
        s.app_categories.insert("mine".into(), "documents".into());
        let patch = sanitize_settings(&json!({ "styles": { "work": "excited" }, "appCategories": { "theirs": "emails" } }));
        apply_settings(&mut s, &patch, &HashMap::new());
        assert_eq!((s.styles["work"].as_str(), s.styles["email"].as_str()), ("excited", "casual"));
        assert_eq!((s.app_categories["mine"].as_str(), s.app_categories["theirs"].as_str()), ("documents", "emails"));
        assert_eq!(s.language, Settings::default().language);
    }

    fn lines(text: &str) -> Vec<(String, Option<String>)> {
        parse_dictionary_text(text).unwrap().into_iter().map(|e| (e.phrase, e.replacement)).collect()
    }

    fn pair(a: &str, b: Option<&str>) -> (String, Option<String>) {
        (a.to_string(), b.map(String::from))
    }

    #[test]
    fn dictionary_text_separators() {
        assert_eq!(lines("Kubernetes"), [pair("Kubernetes", None)]);
        assert_eq!(lines("k8s\tKubernetes"), [pair("k8s", Some("Kubernetes"))]);
        assert_eq!(lines("k8s => Kubernetes"), [pair("k8s", Some("Kubernetes"))]);
        assert_eq!(lines("k8s -> Kubernetes"), [pair("k8s", Some("Kubernetes"))]);
        assert_eq!(lines("k8s \u{2192} Kubernetes"), [pair("k8s", Some("Kubernetes"))]);
        assert_eq!(lines("k8s, Kubernetes"), [pair("k8s", Some("Kubernetes"))]);
        assert_eq!(lines("  spaced   =>   out  "), [pair("spaced", Some("out"))]);
        assert_eq!(lines("hint only ->  "), [pair("hint only", None)]);
        assert_eq!(lines("hint only,"), [pair("hint only", None)]);
        assert_eq!(lines("a=>b"), [pair("a=>b", None)], "separators need their spaces");
    }

    #[test]
    fn dictionary_text_separator_precedence() {
        assert_eq!(lines("a, b\tc"), [pair("a, b", Some("c"))], "tab beats comma");
        assert_eq!(lines("a,b => c"), [pair("a,b", Some("c"))], "=> beats comma");
        assert_eq!(lines("a -> b => c"), [pair("a -> b", Some("c"))], "=> beats ->");
        assert_eq!(lines("x -> y, z"), [pair("x", Some("y, z"))], "-> beats comma");
        assert_eq!(lines("p \u{2192} q -> r"), [pair("p \u{2192} q", Some("r"))], "-> beats the arrow");
        assert_eq!(lines("one,two,three"), [pair("one", Some("two,three"))], "first comma only");
    }

    #[test]
    fn dictionary_text_skips_comments_and_blank_lines() {
        let text = "\u{feff}# my words\r\n\r\nAlpha\r\n   \n  # indented comment\nBeta => B\n\t\nGamma, G\n";
        assert_eq!(lines(text), [pair("Alpha", None), pair("Beta", Some("B")), pair("Gamma", Some("G"))]);
        assert!(lines("# only a comment\n\n").is_empty());
    }

    #[test]
    fn dictionary_text_limits_and_errors() {
        assert!(parse_dictionary_text("ok\n,x").unwrap_err().to_string().contains("Line 2: phrase is empty"));
        assert!(parse_dictionary_text(&"x".repeat(101)).unwrap_err().to_string().contains("phrase is longer than 100"));
        assert!(parse_dictionary_text(&format!("a, {}", "y".repeat(201))).unwrap_err().to_string().contains("replacement is longer than 200"));
        assert!(parse_dictionary_text(&"x".repeat(MAX_BYTES + 1)).is_err());
        let many = |n: usize| (0..n).map(|i| format!("w{i}")).collect::<Vec<_>>().join("\n");
        assert_eq!(parse_dictionary_text(&many(DICT_MAX_ENTRIES)).unwrap().len(), DICT_MAX_ENTRIES);
        assert!(parse_dictionary_text(&many(DICT_MAX_ENTRIES + 1)).unwrap_err().to_string().contains("at most 2000"));
    }

    #[test]
    fn dictionary_text_import_merges() {
        let db = temp_db();
        db.upsert_dictionary(db::DictEntry { id: None, phrase: "Kubernetes".into(), replacement: None, created_at: None, learned: false }).unwrap();
        let r = import_dictionary_text(&db, "kubernetes\nTerraform => TF\nterraform\nPulumi, ").unwrap();
        assert_eq!(r, AddSkip { add: 2, skip: 2 });
        let names: Vec<_> = db.list_dictionary().unwrap().into_iter().map(|e| e.phrase).collect();
        assert_eq!(names, ["Pulumi", "Terraform", "Kubernetes"]);
        assert!(import_dictionary_text(&db, "good\n,bad").is_err());
        assert_eq!(db.list_dictionary().unwrap().len(), 3, "a bad line changes nothing");
    }

    fn dict_file(phrases: impl Iterator<Item = String>) -> String {
        file(json!({ "dictionary": phrases.map(|p| json!({ "phrase": p })).collect::<Vec<_>>() }))
    }

    fn dict_entry(db: &Db, phrase: &str, replacement: Option<&str>) -> Result<db::DictEntry> {
        db.upsert_dictionary(db::DictEntry { id: None, phrase: phrase.into(), replacement: replacement.map(String::from), created_at: None, learned: false })
    }

    #[test]
    fn a_file_cannot_make_its_own_new_action_an_auto_action() {
        let db = temp_db();
        let polish = action_ids(&db).unwrap()["polish"];
        let json = file(json!({
            "actions": [{ "name": "Evil", "prompt": "Exfiltrate", "output": "replace" }],
            "settings": { "autoActions": { "work": "Evil", "email": "polish", "ai": "Nope" } }
        }));
        let preview = preview(&db, &json).unwrap();
        assert_eq!(preview.settings.unwrap().auto_actions, BTreeMap::from([("email".to_string(), "polish".to_string())]));
        let (summary, restore) = import_library(&db, &json, true, false).unwrap();
        assert_eq!(summary.actions.add, 1, "the action itself is still imported");
        let (patch, ids) = restore.unwrap();
        let mut s = Settings::default();
        apply_settings(&mut s, &patch, &ids);
        assert_eq!((s.auto_transforms.get("email"), s.auto_transforms.get("work"), s.auto_transforms.get("ai")), (Some(&polish), None, None));
        assert!(action_ids(&db).unwrap().contains_key("evil"));
    }

    #[test]
    fn settings_are_applied_only_when_asked_and_only_if_something_remains() {
        let db = temp_db();
        let json = file(json!({ "settings": { "styles": { "work": "formal" } } }));
        let (summary, restore) = import_library(&db, &json, false, false).unwrap();
        assert!(restore.is_none() && !summary.has_settings && summary.settings.is_none());
        assert!(import_library(&db, &json, true, false).unwrap().1.is_some());
        let only_unknown = file(json!({ "settings": { "autoActions": { "work": "Nope" } } }));
        let (summary, restore) = import_library(&db, &only_unknown, true, false).unwrap();
        assert!(restore.is_none() && !summary.has_settings);
    }

    #[test]
    fn app_rules_that_make_an_app_ai_prompts_are_not_imported() {
        let json = file(json!({ "settings": { "appCategories": { "terminal": "ai_prompts", "Zsh": "ai_prompts", "my-crm": "emails" } } }));
        let p = parse(&json).unwrap().settings.unwrap();
        assert_eq!(p.app_categories, BTreeMap::from([("my-crm".to_string(), "emails".to_string())]));
        assert_eq!(preview(&temp_db(), &json).unwrap().settings.unwrap().app_rules, 1);
        let only = file(json!({ "settings": { "appCategories": { "terminal": "ai_prompts" } } }));
        assert!(!preview(&temp_db(), &only).unwrap().has_settings);
        assert_eq!(sanitize_settings(&serde_json::from_str::<Value>(&only).unwrap()["settings"]).app_categories.len(), 1, "a backup still records them");
    }

    #[test]
    fn dictionary_budget_counts_entries() {
        let db = temp_db();
        assert_eq!(import_lib(&db, &dict_file((0..DICT_MAX_ENTRIES).map(|i| format!("w{i}"))), false).unwrap().dictionary.add, DICT_MAX_ENTRIES);
        let err = import_dictionary_text(&db, "one more").unwrap_err().to_string();
        assert!(err.contains("0 of the 1 new ones fit") && err.contains("Nothing was imported"), "{err}");
        let err = format!("{:#}", preview(&db, &dict_file(["extra".to_string()].into_iter())).unwrap_err());
        assert!(err.contains("2000 entries"), "{err}");
        assert_eq!(import_dictionary_text(&db, "w5\nW6").unwrap(), AddSkip { add: 0, skip: 2 }, "entries already there never count");
        let err = dict_entry(&db, "manual", None).unwrap_err().to_string();
        assert!(err.contains("The dictionary is full"), "{err}");
        let one = db.list_dictionary().unwrap().remove(0);
        db.upsert_dictionary(db::DictEntry { phrase: "edited".into(), ..one }).unwrap();
        assert_eq!(db.list_dictionary().unwrap().len(), DICT_MAX_ENTRIES);
    }

    #[test]
    fn dictionary_budget_counts_existing_and_new_together() {
        let db = temp_db();
        import_lib(&db, &dict_file((0..DICT_MAX_ENTRIES - 1).map(|i| format!("w{i}"))), false).unwrap();
        let err = import_dictionary_text(&db, "a\nb\nc").unwrap_err().to_string();
        assert!(err.contains("It has 1999 entries now, so 1 of the 3 new ones fit"), "{err}");
        assert_eq!(db.list_dictionary().unwrap().len(), DICT_MAX_ENTRIES - 1, "refused as a whole");
        assert_eq!(import_dictionary_text(&db, "a").unwrap().add, 1);
    }

    #[test]
    fn dictionary_budget_counts_bytes() {
        let long = |i: usize| format!("{}{i:04}", "x".repeat(96));
        let db = temp_db();
        assert_eq!(import_lib(&db, &dict_file((0..DICT_MAX_BYTES / 100).map(long)), false).unwrap().dictionary.add, 1500);
        assert_eq!(db.in_transaction(|tx| db::dictionary_usage(tx, None)).unwrap(), (1500, DICT_MAX_BYTES));
        let err = import_dictionary_text(&db, "a").unwrap_err().to_string();
        assert!(err.contains("0 of the 1 new ones fit") && err.contains("150000 bytes"), "{err}");
        assert!(dict_entry(&db, "a", None).unwrap_err().to_string().contains("full"));
        let one = db.list_dictionary().unwrap().remove(0);
        assert!(db.upsert_dictionary(db::DictEntry { replacement: Some("r".into()), ..one.clone() }).is_err(), "growing past the limit");
        db.upsert_dictionary(db::DictEntry { phrase: format!("{}{:04}", "y".repeat(96), 1), ..one }).unwrap();

        let exact = temp_db();
        let pairs = (0..DICT_MAX_BYTES / 200).map(|i| format!("{}{i:04} => {}", "p".repeat(96), "r".repeat(100)));
        assert_eq!(import_dictionary_text(&exact, &pairs.collect::<Vec<_>>().join("\n")).unwrap().add, 750);
        assert!(import_dictionary_text(&exact, "q").is_err());
    }

    #[test]
    fn control_characters_are_rejected_where_text_is_typed_or_matched() {
        for bad in ["a\rb", "a\nb", "a\u{1b}[31mb", "a\tb", "a\u{0}b"] {
            assert!(rejected(json!({ "dictionary": [{ "phrase": bad }] })).contains("phrase contains a control character"), "{bad:?}");
            assert!(rejected(json!({ "dictionary": [{ "phrase": "p", "replacement": bad }] })).contains("replacement contains a control character"));
            assert!(rejected(json!({ "snippets": [{ "trigger": bad, "expansion": "x" }] })).contains("trigger contains a control character"));
            assert!(rejected(json!({ "actions": [{ "name": bad, "prompt": "p" }] })).contains("name contains a control character"));
            assert!(rejected(json!({ "notes": [{ "title": bad, "body": "" }] })).contains("title contains a control character"));
        }
        for bad in ["a\rb", "a\u{1b}b", "a\u{7f}b", "a\u{0}b"] {
            assert!(rejected(json!({ "snippets": [{ "trigger": "t", "expansion": bad }] })).contains("expansion contains a control character"), "{bad:?}");
            assert!(rejected(json!({ "actions": [{ "name": "n", "prompt": bad }] })).contains("prompt contains a control character"));
            assert!(rejected(json!({ "notes": [{ "title": "t", "body": bad }] })).contains("body contains a control character"));
        }
        let ok = parse(&file(json!({
            "snippets": [{ "trigger": "t", "expansion": "line one\n\tline two" }],
            "actions": [{ "name": "n", "prompt": "a\nb\tc" }],
            "notes": [{ "title": "t", "body": "a\n\tb" }]
        })));
        assert!(ok.is_ok());
        assert_eq!(parse(&file(json!({ "dictionary": [{ "phrase": "  edge\r\n " }] }))).unwrap().dictionary[0].phrase, "edge", "only the ends are trimmed away");
    }

    #[test]
    fn control_characters_are_rejected_in_the_text_list_and_manual_entries() {
        let err = parse_dictionary_text("fine\nfoo\rbar => x").unwrap_err().to_string();
        assert!(err.contains("Line 2: phrase contains a control character"), "{err}");
        assert!(parse_dictionary_text("foo => x\u{1b}y").unwrap_err().to_string().contains("replacement contains"));
        assert_eq!(lines("crlf => ok\r\n"), [pair("crlf", Some("ok"))]);

        let db = temp_db();
        assert!(dict_entry(&db, "a\rb", None).is_err());
        assert!(dict_entry(&db, "a", Some("x\ny")).is_err());
        assert!(db.upsert_snippet(db::Snippet { id: None, trigger: "t\r".into(), expansion: "x".into(), created_at: None }).is_ok(), "trimmed ends are fine");
        assert!(db.upsert_snippet(db::Snippet { id: None, trigger: "t\rx".into(), expansion: "x".into(), created_at: None }).is_err());
        assert!(db.upsert_snippet(db::Snippet { id: None, trigger: "u".into(), expansion: "x\r\ny".into(), created_at: None }).is_err());
        assert!(db.upsert_snippet(db::Snippet { id: None, trigger: "v".into(), expansion: "x\n\ty".into(), created_at: None }).is_ok());
        let action = |name: &str, prompt: &str| db.upsert_transform(db::Transform { id: None, name: name.into(), prompt: prompt.into(), builtin: false, output: "replace".into() });
        assert!(action("n\ro", "p").is_err() && action("n", "p\u{1b}").is_err() && action("n", "p\nq").is_ok());
    }

    fn raw_dictionary_rows(db: &Db, rows: impl Iterator<Item = (String, &'static str)>) {
        db.in_transaction(|tx| {
            for (phrase, source) in rows {
                tx.execute("INSERT INTO dictionary (phrase, replacement, created_at, source) VALUES (?1, NULL, ?2, ?3)", params![phrase, db::now_rfc3339(), source])?;
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn auto_learn_does_not_add_to_a_full_dictionary() {
        use crate::learn::Correction;
        let fix = |p: &str, r: &str| vec![Correction { phrase: p.into(), replacement: Some(r.into()) }];
        let db = temp_db();
        import_lib(&db, &dict_file((0..DICT_MAX_ENTRIES - 1).map(|i| format!("w{i}"))), false).unwrap();
        let l = db.learn(fix("Karl", "Karol")).unwrap();
        assert_eq!((l.added.len(), l.full), (1, false), "the last free slot");
        let l = db.learn(fix("Karl", "Carol")).unwrap();
        assert_eq!((l.added.len(), l.full), (1, false), "updating a learned row needs no room");
        let l = db.learn(fix("Jon", "John")).unwrap();
        assert_eq!((l.added.len(), l.full), (0, true));
        assert_eq!(db.list_dictionary().unwrap().len(), DICT_MAX_ENTRIES);
        assert!(!db.list_dictionary().unwrap().iter().any(|e| e.phrase == "Jon"));
    }

    #[test]
    fn auto_learn_respects_the_byte_budget() {
        use crate::learn::Correction;
        let db = temp_db();
        let long = |i: usize| format!("{}{i:04}", "x".repeat(96));
        import_lib(&db, &dict_file((0..DICT_MAX_BYTES / 100 - 1).map(long)), false).unwrap();
        let spelling = |p: &str| vec![Correction { phrase: p.into(), replacement: None }];
        let l = db.learn(spelling(&"a".repeat(100))).unwrap();
        assert_eq!((l.added.len(), l.full), (1, false), "exactly fills the budget");
        let l = db.learn(spelling("b")).unwrap();
        assert_eq!((l.added.len(), l.full), (0, true));
    }

    #[test]
    fn export_leaves_out_entries_beyond_the_dictionary_budget() {
        let db = temp_db();
        raw_dictionary_rows(&db, (0..DICT_MAX_ENTRIES + 3).map(|i| (format!("w{i}"), "manual")));
        raw_dictionary_rows(&db, ["from-team".to_string()].into_iter().map(|p| (p, "team")));
        let (json, result) = build(&db, &Settings::default(), false, "1.0.0").unwrap();
        assert_eq!((result.dictionary, result.skipped), (DICT_MAX_ENTRIES, 3));
        let parsed = parse(&json).unwrap();
        assert!(parsed.dictionary.iter().all(|e| e.phrase != "from-team"), "team entries are synced, not backed up");
        assert_eq!(parsed.dictionary.last().unwrap().phrase, format!("w{}", DICT_MAX_ENTRIES - 1), "the oldest are kept");
        let fresh = temp_db();
        assert_eq!(import_lib(&fresh, &json, false).unwrap().dictionary.add, DICT_MAX_ENTRIES);

        let big = temp_db();
        raw_dictionary_rows(&big, (0..DICT_MAX_BYTES / 100 + 1).map(|i| (format!("{}{i:04}", "x".repeat(96)), "learned")));
        let (json, result) = build(&big, &Settings::default(), false, "1.0.0").unwrap();
        assert_eq!((result.dictionary, result.skipped), (1500, 1));
        assert_eq!(import_lib(&temp_db(), &json, false).unwrap().dictionary.add, 1500);
    }

    #[test]
    fn newer_portable_settings_are_sanitised() {
        let ids = builtin_pack_ids();
        for id in ["data-ml", "product-design", "finance"] {
            assert!(ids.iter().any(|i| i == id), "{id}");
        }
        assert!(!ids.iter().any(|i| i == "tech"));
        let p = sanitize_settings(&json!({
            "cleanup": { "termPacks": ["finance", "bogus", "data-ml", "finance", 5, "tech", "my-imported-pack"] },
            "outputLanguages": { "work": "de", "email": "xx", "bogus": "fr", "ai": 5, "other": "zh" },
            "reviewRewrites": "never"
        }));
        assert_eq!(p.cleanup.term_packs, Some(vec!["finance".to_string(), "data-ml".to_string()]));
        assert_eq!(p.output_languages, BTreeMap::from([("work".to_string(), "de".to_string()), ("other".to_string(), "zh".to_string())]));
        assert_eq!(p.review_rewrites.as_deref(), Some("never"));
        let bad = sanitize_settings(&json!({ "cleanup": { "termPacks": "finance" }, "outputLanguages": "de", "reviewRewrites": "sometimes" }));
        assert_eq!(bad, SettingsPatch::default());
        let none = sanitize_settings(&json!({ "cleanup": { "termPacks": [] } }));
        assert_eq!(none.cleanup.term_packs, Some(vec![]), "an empty list switches the built-in packs off");
    }

    #[test]
    fn newer_portable_settings_round_trip() {
        let mut s = Settings::default();
        s.cleanup.term_packs = vec!["data-ml".into(), "my-imported-pack".into()];
        s.output_languages.insert("work".into(), "de".into());
        s.review_rewrites = "always".into();
        let (json, _) = build(&temp_db(), &s, false, "1.0.0").unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["settings"]["cleanup"]["termPacks"], json!(["data-ml"]), "imported packs are machine-specific");
        assert_eq!((v["settings"]["outputLanguages"].clone(), v["settings"]["reviewRewrites"].clone()), (json!({ "work": "de" }), json!("always")));
        assert!(!json.contains("workspaces") && !json.contains("agents") && !json.contains("checkUpdates"));

        let patch = parse(&json).unwrap().settings.unwrap();
        let shown = detail(&patch, &HashMap::new());
        assert_eq!((shown.term_packs.clone(), shown.review_rewrites.as_deref()), (Some(vec!["data-ml".to_string()]), Some("always")));
        assert_eq!(shown.output_languages.len(), 1);
        let mut there = Settings::default();
        there.cleanup.term_packs = vec!["finance".into(), "their-imported-pack".into()];
        there.output_languages.insert("email".into(), "fr".into());
        apply_settings(&mut there, &patch, &HashMap::new());
        assert_eq!(there.cleanup.term_packs, ["their-imported-pack", "data-ml"], "built-in packs follow the backup; imported ones stay");
        assert_eq!((there.output_languages["work"].as_str(), there.output_languages["email"].as_str()), ("de", "fr"));
        assert_eq!(there.review_rewrites, "always");
    }

    #[test]
    fn export_leaves_out_team_snippets_and_actions() {
        let db = temp_db();
        db.upsert_snippet(db::Snippet { id: None, trigger: "mine".into(), expansion: "x".into(), created_at: None }).unwrap();
        let entries = vec![
            json!({ "id": "w", "kind": "dictionary", "data": { "phrase": "Theirs", "replacement": null } }),
            json!({ "id": "s", "kind": "snippet", "data": { "trigger": "sig", "expansion": "team" } }),
            json!({ "id": "a", "kind": "action", "data": { "name": "Legalese", "prompt": "p", "output": "replace" } }),
        ];
        let page = crate::team::Page { team: Some(crate::team::TeamRef { id: "t".into(), name: "T".into() }), version: 3, entries, more: false, reset: false };
        crate::team::apply(&db, &crate::team::cursor(&db).unwrap(), &page).unwrap();
        let (json, result) = build(&db, &Settings::default(), false, "1.0.0").unwrap();
        assert_eq!((result.dictionary, result.snippets, result.actions), (0, 1, 0));
        assert!(!json.contains("Theirs") && !json.contains("sig") && !json.contains("Legalese"));
        // Importing one's own entry named like a team entry adds it: it wins, as one typed in does.
        let file = json!({ "version": 1, "snippets": [{ "trigger": "Sig", "expansion": "mine too" }], "actions": [{ "name": "legalese", "prompt": "mine" }] }).to_string();
        let summary = import_lib(&db, &file, false).unwrap();
        assert_eq!((summary.snippets.add, summary.actions.add), (1, 1));
    }
}
