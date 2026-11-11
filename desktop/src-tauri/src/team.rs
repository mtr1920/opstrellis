//! The team library: dictionary entries, snippets and voice actions that a team's admins edit on the
//! server, synced down read-only from `GET /api/library` (server/README.md, "Library sync"). They
//! live in the user's own tables, marked by `team_id`; `db.rs` decides which ones are used. Server
//! data is untrusted here: every entry is checked like a backup entry, and invalid ones are skipped.

use crate::backup::{action_item, dict_item, snippet_item, ActionItem, DictItem, SnippetItem};
use crate::db::{now_rfc3339, plain_line, Db};
use crate::stt::{send, HTTP};
use crate::App;
use anyhow::{anyhow, ensure, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use tauri::Emitter;

pub const INTERVAL: Duration = Duration::from_secs(5 * 60);
const MAX_WAIT: Duration = Duration::from_secs(60 * 60);
const RATE_LIMITED_WAIT: Duration = Duration::from_secs(15 * 60);
const MAX_BODY: usize = 16 * 1024 * 1024;
/// Pages per run; an incremental page holds at most 500 changes, and the next run goes on from the cursor.
const MAX_PAGES: usize = 20;
const MAX_ID: usize = 64;
/// The server's limit on live entries: no page holds more, and a hostile server can't fill the disk
/// (team rows plus skipped ids never pass it by more than a small margin).
const MAX_ENTRIES: usize = 2000;
const MAX_STORED: i64 = MAX_ENTRIES as i64 + 100;
const MAX_ERROR_BODY: usize = 64 * 1024;
const MAX_TEAM_NAME: usize = 80;

/// One sync at a time (background loop, sign-in, "Sync now"), so each request is sent with the cursor its answer is applied to.
static RUNNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A `GET /api/library` response.
#[derive(Debug, Deserialize)]
pub struct Page {
    pub team: Option<TeamRef>,
    pub version: u64,
    #[serde(default)]
    pub entries: Vec<Value>,
    #[serde(default)]
    pub more: bool,
    #[serde(default)]
    pub reset: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamRef {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize)]
struct RawEntry {
    id: String,
    kind: String,
    #[serde(default)]
    data: Value,
    #[serde(default)]
    deleted: bool,
}

enum Item {
    Dictionary(DictItem),
    Snippet(SnippetItem),
    Action(ActionItem),
}

/// What the next request is sent with, and what its answer must still find to be applied: `forget`
/// bumps `epoch`, so an answer to a request sent before a sign-out is dropped.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Cursor {
    pub epoch: i64,
    pub team_id: Option<String>,
    pub version: i64,
}

#[derive(Debug, PartialEq)]
pub enum Step {
    /// The cursor is current.
    Done,
    /// Request again at once with the stored cursor (`more`, or the team changed).
    Again,
    /// The cursor changed while the request was out (signed out, setting off): nothing was applied.
    Stale,
}

#[derive(Debug, PartialEq)]
pub struct Applied {
    pub step: Step,
    /// Entries or the team changed, so the library pages should reload.
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamStatus {
    pub team: Option<TeamRef>,
    pub dictionary: usize,
    pub snippets: usize,
    pub actions: usize,
    /// Entries not used because they are invalid here or past a limit (not those the user's own shadow).
    pub skipped: usize,
    pub synced_at: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug)]
pub enum Failure {
    SignedOut,
    RateLimited,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for Failure {
    fn from(e: anyhow::Error) -> Self {
        Failure::Other(e)
    }
}

fn read_cursor(c: &Connection) -> Result<Cursor> {
    Ok(c.query_row("SELECT epoch, team_id, version FROM team_sync WHERE id = 1", [], |r| Ok(Cursor { epoch: r.get(0)?, team_id: r.get(1)?, version: r.get(2)? }))?)
}

pub fn cursor(db: &Db) -> Result<Cursor> {
    db.in_transaction(|tx| read_cursor(tx))
}

/// Deletes every local team entry; true if there were any.
fn clear_entries(c: &Connection) -> Result<bool> {
    let n = c.execute("DELETE FROM dictionary WHERE source = 'team' OR team_id IS NOT NULL", [])?
        + c.execute("DELETE FROM snippets WHERE team_id IS NOT NULL", [])?
        + c.execute("DELETE FROM transforms WHERE team_id IS NOT NULL", [])?
        + c.execute("DELETE FROM team_skipped", [])?;
    Ok(n > 0)
}

/// An entry's final state checked with the rules the user's own entries and backups follow.
fn item(kind: &str, data: Value) -> Option<Item> {
    match kind {
        "dictionary" => Some(Item::Dictionary(dict_item(serde_json::from_value(data).ok()?).ok()?)),
        "snippet" => {
            let s = snippet_item(serde_json::from_value(data).ok()?).ok()?;
            // It would type the user's clipboard wherever someone says the trigger.
            (!s.expansion.contains("{clipboard}")).then_some(Item::Snippet(s))
        }
        "action" => Some(Item::Action(action_item(serde_json::from_value(data).ok()?).ok()?)),
        _ => None,
    }
}

/// Upserts one entry by id (or deletes it); an invalid one is removed and remembered as skipped.
fn apply_entry(c: &Connection, raw: &Value) -> Result<()> {
    let Ok(e) = RawEntry::deserialize(raw) else { return Ok(()) };
    if e.id.is_empty() || e.id.len() > MAX_ID || !plain_line(&e.id) {
        return Ok(());
    }
    let item = if e.deleted { None } else { item(&e.kind, e.data) };
    let table = match &item {
        Some(Item::Dictionary(_)) => "dictionary",
        Some(Item::Snippet(_)) => "snippets",
        Some(Item::Action(_)) => "transforms",
        None => "",
    };
    for other in ["dictionary", "snippets", "transforms"].into_iter().filter(|t| *t != table) {
        c.execute(&format!("DELETE FROM {other} WHERE team_id = ?1"), [&e.id])?;
    }
    if item.is_some() || e.deleted {
        c.execute("DELETE FROM team_skipped WHERE id = ?1", [&e.id])?;
    } else {
        c.execute("INSERT OR IGNORE INTO team_skipped (id) VALUES (?1)", [&e.id])?;
    }
    // Updated in place, so a dictionary entry keeps its usage count and every entry its place in the budget order.
    match item {
        Some(Item::Dictionary(d)) => c.execute(
            "INSERT INTO dictionary (phrase, replacement, created_at, source, team_id) VALUES (?1, ?2, ?3, 'team', ?4)
             ON CONFLICT(team_id) DO UPDATE SET phrase = excluded.phrase, replacement = excluded.replacement",
            params![d.phrase, d.replacement, now_rfc3339(), e.id],
        )?,
        Some(Item::Snippet(s)) => c.execute(
            "INSERT INTO snippets (trigger, expansion, created_at, team_id) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(team_id) DO UPDATE SET trigger = excluded.trigger, expansion = excluded.expansion",
            params![s.trigger, s.expansion, now_rfc3339(), e.id],
        )?,
        Some(Item::Action(a)) => c.execute(
            "INSERT INTO transforms (name, prompt, builtin, output, team_id) VALUES (?1, ?2, 0, ?3, ?4)
             ON CONFLICT(team_id) DO UPDATE SET name = excluded.name, prompt = excluded.prompt, output = excluded.output",
            params![a.name, a.prompt, a.output, e.id],
        )?,
        None => 0,
    };
    Ok(())
}

/// Applies one response to a request sent with `sent`, in one transaction, following the client
/// rule in server/README.md.
pub fn apply(db: &Db, sent: &Cursor, page: &Page) -> Result<Applied> {
    db.in_transaction(|tx| {
        if read_cursor(tx)? != *sent {
            return Ok(Applied { step: Step::Stale, changed: false });
        }
        let now = now_rfc3339();
        let Some(team) = &page.team else {
            let changed = clear_entries(tx)? | sent.team_id.is_some();
            tx.execute("UPDATE team_sync SET team_id = NULL, team_name = NULL, version = 0, synced_at = ?1, error = NULL WHERE id = 1", [&now])?;
            return Ok(Applied { step: Step::Done, changed });
        };
        ensure!(!team.id.is_empty() && team.id.len() <= MAX_ID && plain_line(&team.id), "The server sent an invalid team id.");
        ensure!(page.entries.len() <= MAX_ENTRIES, "The server sent more than {MAX_ENTRIES} team library entries at once.");
        let version = i64::try_from(page.version).map_err(|_| anyhow!("The server sent an invalid library version."))?;
        let name: String = team.name.trim().chars().filter(|c| !c.is_control()).take(MAX_TEAM_NAME).collect();
        let old_name: Option<String> = tx.query_row("SELECT team_name FROM team_sync WHERE id = 1", [], |r| r.get(0))?;
        let mut changed = old_name.as_deref() != Some(name.as_str());
        if sent.team_id.as_deref() != Some(team.id.as_str()) {
            clear_entries(tx)?;
            changed = true;
            // An answer to `since > 0` was computed with the old team's cursor. One to `since = 0` is
            // the new team's full library whatever the cursor was, so it is applied below.
            if sent.version > 0 {
                tx.execute("UPDATE team_sync SET team_id = NULL, team_name = NULL, version = 0 WHERE id = 1", [])?;
                return Ok(Applied { step: Step::Again, changed });
            }
        }
        // A full sync holds every live entry and no tombstones, so nothing local may outlive it.
        if page.reset || sent.version == 0 {
            changed |= clear_entries(tx)?;
        }
        for raw in &page.entries {
            apply_entry(tx, raw)?;
            changed = true;
        }
        // Rolls the whole response back: the library stays as it was and the error is shown.
        ensure!(stored(tx)? <= MAX_STORED, "The server sent more team library entries than a team can have, so the library wasn't updated.");
        tx.execute(
            "UPDATE team_sync SET team_id = ?1, team_name = ?2, version = ?3, synced_at = ?4, error = NULL WHERE id = 1",
            params![team.id, name, version, now],
        )?;
        Ok(Applied { step: if page.more { Step::Again } else { Step::Done }, changed })
    })
}

/// Team rows and skipped ids stored here.
fn stored(c: &Connection) -> Result<i64> {
    Ok(c.query_row(
        "SELECT (SELECT COUNT(*) FROM dictionary WHERE source = 'team') + (SELECT COUNT(*) FROM snippets WHERE team_id IS NOT NULL)
              + (SELECT COUNT(*) FROM transforms WHERE team_id IS NOT NULL) + (SELECT COUNT(*) FROM team_skipped)",
        [],
        |r| r.get(0),
    )?)
}

/// Deletes the team entries and the cursor (signed out, another server, setting off). A sync in
/// flight then finds the cursor changed and drops its answer. True if anything was there.
pub fn forget(db: &Db) -> Result<bool> {
    db.in_transaction(|tx| {
        let had = clear_entries(tx)? | read_cursor(tx)?.team_id.is_some();
        tx.execute("UPDATE team_sync SET epoch = epoch + 1, team_id = NULL, team_name = NULL, version = 0, synced_at = NULL, error = NULL WHERE id = 1", [])?;
        Ok(had)
    })
}

/// `forget`, telling the library pages when something went.
pub fn clear(app: &App) -> Result<()> {
    if forget(&app.db)? {
        let _ = app.handle.emit("team-updated", ());
    }
    Ok(())
}

pub fn status(db: &Db) -> Result<TeamStatus> {
    let (team, synced_at, error, invalid) = db.in_transaction(|tx| {
        let (id, name, synced_at, error): (Option<String>, Option<String>, Option<String>, Option<String>) =
            tx.query_row("SELECT team_id, team_name, synced_at, error FROM team_sync WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        let invalid: i64 = tx.query_row("SELECT COUNT(*) FROM team_skipped", [], |r| r.get(0))?;
        Ok((id.map(|id| TeamRef { id, name: name.unwrap_or_default() }), synced_at, error, invalid as usize))
    })?;
    let limit = |u: Option<&str>| usize::from(matches!(u, Some("limit" | "complex")));
    let (mut skipped, mut dictionary, mut snippets, mut actions, mut complex) = (invalid, 0, 0, 0, false);
    for r in db.list_dictionary_rows()?.into_iter().filter(|r| r.source == "team") {
        dictionary += 1;
        skipped += limit(r.unused);
        complex |= r.unused == Some("complex");
    }
    for r in db.list_snippet_rows()?.into_iter().filter(|r| r.team) {
        snippets += 1;
        skipped += limit(r.unused);
    }
    for r in db.list_transform_rows()?.into_iter().filter(|r| r.team) {
        actions += 1;
        skipped += limit(r.unused);
    }
    let error = error.or_else(|| complex.then(|| "The team's words are too complex to match together with yours, so only yours are used.".to_string()));
    Ok(TeamStatus { team, dictionary, snippets, actions, skipped, synced_at, error })
}

async fn read_capped(mut resp: reqwest::Response, max: usize) -> Result<Vec<u8>> {
    let too_big = || anyhow!("The team library answer is over {} MB.", max >> 20);
    if resp.content_length().is_some_and(|n| n > max as u64) {
        return Err(too_big());
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.without_url())? {
        if body.len() + chunk.len() > max {
            return Err(too_big());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn fetch(base: &str, token: &str, since: i64) -> Result<Page, Failure> {
    let resp = send(HTTP.get(format!("{base}/api/library?since={since}")).bearer_auth(token)).await?;
    let status = resp.status();
    match status.as_u16() {
        401 => return Err(Failure::SignedOut),
        429 => return Err(Failure::RateLimited),
        _ if !status.is_success() => {
            // Like `stt::check`, without reading an unbounded error body.
            let body = read_capped(resp, MAX_ERROR_BODY).await.unwrap_or_default();
            let v: Value = serde_json::from_slice(&body).unwrap_or_default();
            let message = v["message"].as_str().or(v["error"].as_str()).unwrap_or_else(|| status.canonical_reason().unwrap_or(""));
            return Err(Failure::Other(anyhow!("cloud {}: {}", status.as_u16(), message.chars().take(200).collect::<String>())));
        }
        _ => {}
    }
    let body = read_capped(resp, MAX_BODY).await?;
    serde_json::from_slice(&body).map_err(|_| Failure::Other(anyhow!("The server's team library answer isn't valid.")))
}

/// Brings the team library up to date (signed in, setting on; otherwise no request is made). The
/// outcome is also stored for `status`; the error is for the background loop's backoff.
pub async fn sync(app: &App) -> Result<(), Failure> {
    let _one = RUNNING.lock().await;
    let mut changed = false;
    let mut epoch = None;
    let mut sent_with = None;
    let result: Result<(), Failure> = async {
        for _ in 0..MAX_PAGES {
            let sent = cursor(&app.db)?;
            epoch.get_or_insert(sent.epoch);
            // Read after the cursor: signing out, another server or turning this off changes them
            // before `forget` bumps the epoch, so an answer to a request sent with stale state is never
            // applied. The server before the credential: a new server URL is set only after the old
            // one's credential is deleted, so it never goes to a new server.
            let base = crate::cloud::base(app)?;
            let Some(credential) = app.secrets.get("cloud").filter(|_| app.settings().cloud.team_library) else { return Ok(()) };
            let page = fetch(&base, sent_with.insert(credential), sent.version).await?;
            let applied = apply(&app.db, &sent, &page)?;
            changed |= applied.changed;
            if applied.step != Step::Again {
                break;
            }
        }
        Ok(())
    }
    .await;
    let message = match &result {
        Ok(()) => None,
        Err(Failure::SignedOut) => {
            // As `cloud::status` does: the token is no longer valid, unless a sign-in replaced it meanwhile.
            if app.secrets.get("cloud").is_some_and(|now| Some(now) == sent_with) {
                let _ = app.secrets.delete("cloud");
                changed |= forget(&app.db).unwrap_or(false);
                let _ = app.handle.emit("cloud-status", crate::cloud::CloudStatus::default());
            }
            None
        }
        Err(Failure::RateLimited) => Some("The server is busy. Trying again later.".to_string()),
        Err(Failure::Other(e)) => Some(format!("{e:#}").chars().take(300).collect()),
    };
    if let (Some(message), Some(epoch)) = (&message, epoch) {
        log::warn!("team library sync failed: {message}");
        let _ = app.db.in_transaction(|tx| Ok(tx.execute("UPDATE team_sync SET error = ?1 WHERE id = 1 AND epoch = ?2", params![message, epoch])?));
    }
    if changed {
        let _ = app.handle.emit("team-updated", ());
    }
    result
}

/// The wait before the next background sync: the interval after a success, doubling (up to an
/// hour) after failures, and at least 15 minutes after the server said too many requests.
pub fn next_wait(previous: Duration, result: &Result<(), Failure>) -> Duration {
    match result {
        Ok(()) | Err(Failure::SignedOut) => INTERVAL,
        Err(Failure::RateLimited) => (previous * 2).max(RATE_LIMITED_WAIT).min(MAX_WAIT),
        Err(Failure::Other(_)) => (previous * 2).min(MAX_WAIT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DictEntry, Snippet, Transform, DICT_MAX_ENTRIES, TEAM_MAX_ACTIONS, TEAM_MAX_SNIPPETS};
    use serde_json::json;

    fn temp_db() -> Db {
        let dir = std::env::temp_dir().join(format!("opstrellis-team-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(&dir.join("t.db")).unwrap()
    }

    fn page(team: Option<(&str, &str)>, version: u64, entries: Vec<Value>, more: bool, reset: bool) -> Page {
        Page { team: team.map(|(id, name)| TeamRef { id: id.into(), name: name.into() }), version, entries, more, reset }
    }

    fn word(id: &str, phrase: &str, replacement: Option<&str>) -> Value {
        json!({ "id": id, "kind": "dictionary", "data": { "phrase": phrase, "replacement": replacement }, "deleted": false })
    }
    fn snippet(id: &str, trigger: &str, expansion: &str) -> Value {
        json!({ "id": id, "kind": "snippet", "data": { "trigger": trigger, "expansion": expansion }, "deleted": false })
    }
    fn action(id: &str, name: &str, output: &str) -> Value {
        json!({ "id": id, "kind": "action", "data": { "name": name, "prompt": format!("Do {name}."), "output": output }, "deleted": false })
    }
    fn gone(id: &str) -> Value {
        json!({ "id": id, "kind": "dictionary", "data": {}, "deleted": true })
    }

    /// Applies one response to whatever the cursor is now.
    fn sync_with(db: &Db, p: Page) -> Applied {
        apply(db, &cursor(db).unwrap(), &p).unwrap()
    }

    fn team_words(db: &Db) -> Vec<(String, Option<String>)> {
        db.list_dictionary_rows().unwrap().into_iter().filter(|r| r.source == "team").rev().map(|r| (r.entry.phrase, r.entry.replacement)).collect()
    }

    fn own(db: &Db, phrase: &str) -> DictEntry {
        db.upsert_dictionary(DictEntry { id: None, phrase: phrase.into(), replacement: None, created_at: None, learned: false }).unwrap()
    }

    #[test]
    fn first_sync_then_incremental_changes_and_tombstones() {
        let db = temp_db();
        assert_eq!(cursor(&db).unwrap(), Cursor::default());
        let a = sync_with(&db, page(Some(("t1", "Acme")), 3, vec![word("e1", "Kubernetes", None), snippet("e2", "sig", "Best, Acme"), action("e3", "Legalese", "replace")], false, false));
        assert_eq!(a, Applied { step: Step::Done, changed: true });
        assert_eq!(cursor(&db).unwrap(), Cursor { epoch: 0, team_id: Some("t1".into()), version: 3 });
        let st = status(&db).unwrap();
        assert_eq!((st.team.clone().unwrap().name.as_str(), st.dictionary, st.snippets, st.actions, st.skipped), ("Acme", 1, 1, 1, 0));
        assert!(st.synced_at.is_some() && st.error.is_none());
        assert!(db.list_dictionary().unwrap().iter().any(|e| e.phrase == "Kubernetes"), "used for dictation");
        assert_eq!(db.list_snippets().unwrap()[0].expansion, "Best, Acme");
        assert!(db.team_action("legalese").unwrap().is_some());

        // Final states by id: an edit, a tombstone, a new entry.
        let a = sync_with(&db, page(Some(("t1", "Acme")), 6, vec![word("e1", "Kubernetes", Some("K8s")), gone("e2"), word("e4", "Postgres", None)], false, false));
        assert_eq!(a, Applied { step: Step::Done, changed: true });
        assert_eq!(team_words(&db), vec![("Kubernetes".into(), Some("K8s".into())), ("Postgres".into(), None)]);
        assert!(db.list_snippets().unwrap().is_empty());
        assert_eq!(cursor(&db).unwrap().version, 6);

        // Nothing new.
        assert_eq!(sync_with(&db, page(Some(("t1", "Acme")), 6, vec![], false, false)), Applied { step: Step::Done, changed: false });
    }

    #[test]
    fn pages_while_more() {
        let db = temp_db();
        sync_with(&db, page(Some(("t1", "Acme")), 2, vec![word("e1", "one", None)], false, false));
        assert_eq!(sync_with(&db, page(Some(("t1", "Acme")), 4, vec![word("e2", "two", None)], true, false)).step, Step::Again);
        assert_eq!(cursor(&db).unwrap().version, 4, "the next request continues after the last entry");
        assert_eq!(sync_with(&db, page(Some(("t1", "Acme")), 5, vec![word("e3", "three", None)], false, false)).step, Step::Done);
        assert_eq!(team_words(&db).len(), 3);
    }

    #[test]
    fn reset_drops_what_the_full_list_leaves_out() {
        let db = temp_db();
        sync_with(&db, page(Some(("t1", "Acme")), 2, vec![word("e1", "one", None), word("e2", "two", None)], false, false));
        // e.g. `since` was above the server's version after a restore, or below the tombstone floor.
        let a = sync_with(&db, page(Some(("t1", "Acme")), 1, vec![word("e2", "two", None)], false, true));
        assert_eq!(a.step, Step::Done);
        assert_eq!(team_words(&db), vec![("two".into(), None)]);
        assert_eq!(cursor(&db).unwrap().version, 1);
    }

    #[test]
    fn another_team_starts_over_and_no_team_clears() {
        let db = temp_db();
        sync_with(&db, page(Some(("t1", "Acme")), 2, vec![word("e1", "one", None)], false, false));
        // Answered for `since=2` by another team: discarded, and synced again from 0.
        let a = sync_with(&db, page(Some(("t2", "Beta")), 9, vec![word("x1", "stale", None)], false, false));
        assert_eq!(a, Applied { step: Step::Again, changed: true });
        assert!(team_words(&db).is_empty());
        assert_eq!(cursor(&db).unwrap(), Cursor { epoch: 0, team_id: None, version: 0 });
        assert_eq!(sync_with(&db, page(Some(("t2", "Beta")), 9, vec![word("x2", "beta", None)], false, false)).step, Step::Done);
        assert_eq!(team_words(&db), vec![("beta".into(), None)]);
        assert_eq!(status(&db).unwrap().team.unwrap().name, "Beta");

        let a = sync_with(&db, page(None, 0, vec![], false, false));
        assert_eq!(a, Applied { step: Step::Done, changed: true });
        assert!(team_words(&db).is_empty() && status(&db).unwrap().team.is_none());
        assert_eq!(sync_with(&db, page(None, 0, vec![], false, false)).changed, false, "still not in a team");
    }

    #[test]
    fn entries_are_final_states_not_unique_by_key() {
        let db = temp_db();
        sync_with(&db, page(Some(("t1", "Acme")), 2, vec![word("e1", "alpha", None), snippet("s1", "sig", "A")], false, false));
        // e1 was renamed away and e2 now has its old phrase; the order within a page does not matter.
        sync_with(&db, page(Some(("t1", "Acme")), 4, vec![word("e2", "alpha", Some("Alpha")), word("e1", "beta", None), snippet("s2", "sig", "B"), json!({ "id": "s1", "kind": "snippet", "data": {}, "deleted": true })], false, false));
        assert_eq!(team_words(&db), vec![("beta".into(), None), ("alpha".into(), Some("Alpha".into()))]);
        assert_eq!(db.list_snippets().unwrap()[0].expansion, "B");
        // A malicious server changing an entry's kind moves it rather than keeping two.
        sync_with(&db, page(Some(("t1", "Acme")), 5, vec![snippet("e1", "beta", "now a snippet")], false, false));
        assert_eq!(team_words(&db), vec![("alpha".into(), Some("Alpha".into()))]);
        assert_eq!(db.list_snippets().unwrap().len(), 2);
    }

    #[test]
    fn invalid_entries_are_skipped_and_counted() {
        let db = temp_db();
        let long = "x".repeat(101);
        sync_with(&db, page(Some(("t1", "Acme")), 9, vec![
            word("ok", "fine", None),
            word("cr", "press\renter", None),
            word("long", &long, None),
            json!({ "id": "rep", "kind": "dictionary", "data": { "phrase": "a", "replacement": "b\u{1b}[31m" } }),
            snippet("nl", "two\nlines", "x"),
            snippet("bell", "t", "ding\u{7}"),
            snippet("clip", "paste it", "{clipboard}"),
            action("out", "Mail it", "email"),
            json!({ "id": "shape", "kind": "dictionary", "data": "Kubernetes" }),
            json!({ "id": "shape2", "kind": "action", "data": { "name": "x" } }),
            json!({ "id": "kind", "kind": "style", "data": { "tone": "formal" } }),
            json!({ "kind": "dictionary", "data": { "phrase": "no id" } }),
            json!("not an object"),
        ], false, false));
        assert_eq!(team_words(&db), vec![("fine".into(), None)]);
        assert!(db.list_snippet_rows().unwrap().is_empty() && !db.list_transform_rows().unwrap().iter().any(|r| r.team));
        assert_eq!(status(&db).unwrap().skipped, 10, "entries without an id can't be told apart, so they aren't counted");
        // Fixed on the server: no longer skipped. Deleted: forgotten.
        sync_with(&db, page(Some(("t1", "Acme")), 10, vec![word("cr", "press enter", None), gone("long")], false, false));
        assert_eq!(status(&db).unwrap().skipped, 8);
        // A valid entry edited into an invalid one is removed, not kept in its old state.
        sync_with(&db, page(Some(("t1", "Acme")), 11, vec![word("ok", "bad\u{0}", None)], false, false));
        assert!(!team_words(&db).iter().any(|w| w.0 == "fine"));
        assert_eq!(status(&db).unwrap().skipped, 9);
    }

    #[test]
    fn a_bad_team_or_version_applies_nothing() {
        let db = temp_db();
        assert!(apply(&db, &cursor(&db).unwrap(), &page(Some(("", "x")), 1, vec![word("e1", "a", None)], false, false)).is_err());
        assert!(apply(&db, &cursor(&db).unwrap(), &page(Some(("t1", "x")), u64::MAX, vec![word("e1", "a", None)], false, false)).is_err());
        assert!(team_words(&db).is_empty());
        assert!(serde_json::from_str::<Page>(r#"{"team":null,"entries":[]}"#).is_err(), "a version is required");
        let p: Page = serde_json::from_str(r#"{"team":{"id":"t","name":"A\u0007cme"},"version":1,"entries":[],"more":false,"reset":false}"#).unwrap();
        sync_with(&db, p);
        assert_eq!(status(&db).unwrap().team.unwrap().name, "Acme", "control characters are dropped from the name");
    }

    #[test]
    fn team_dictionary_fills_only_what_the_users_own_leave() {
        let db = temp_db();
        for i in 0..DICT_MAX_ENTRIES - 3 {
            own(&db, &format!("own{i}"));
        }
        let entries = (0..5).map(|i| word(&format!("e{i}"), &format!("team{i}"), None)).collect();
        sync_with(&db, page(Some(("t1", "Acme")), 5, entries, false, false));
        let used: Vec<_> = db.list_dictionary().unwrap().into_iter().filter(|e| e.phrase.starts_with("team")).map(|e| e.phrase).collect();
        assert_eq!(used, ["team2", "team1", "team0"], "in server order until the budget is reached");
        assert_eq!(db.list_dictionary().unwrap().len(), DICT_MAX_ENTRIES);
        let st = status(&db).unwrap();
        assert_eq!((st.dictionary, st.skipped), (5, 2));
        assert!(!db.vocabulary(10_000, 0, |_| false, &[]).unwrap().iter().any(|w| w == "team3" || w == "team4"));
        // The user's own entries still fit: team entries never take their room.
        own(&db, "mine");
        assert_eq!(status(&db).unwrap().skipped, 3);
        // By bytes too.
        let db = temp_db();
        own(&db, &"y".repeat(100));
        let big = (0..800).map(|i| word(&format!("b{i}"), &format!("{i:03}{}", "z".repeat(97)), Some(&"r".repeat(200)))).collect();
        sync_with(&db, page(Some(("t1", "Acme")), 800, big, false, false));
        let fit = (crate::db::DICT_MAX_BYTES - 100) / 300;
        assert_eq!(status(&db).unwrap().skipped, 800 - fit);
    }

    #[test]
    fn snippets_and_actions_have_caps() {
        let db = temp_db();
        let mut entries: Vec<Value> = (0..TEAM_MAX_ACTIONS + 2).map(|i| action(&format!("a{i}"), &format!("Action {i}"), "copy")).collect();
        entries.extend((0..TEAM_MAX_SNIPPETS + 1).map(|i| snippet(&format!("s{i}"), &format!("trigger {i}"), "x")));
        sync_with(&db, page(Some(("t1", "Acme")), 1, entries, false, false));
        let rows = db.list_transform_rows().unwrap();
        assert_eq!(rows.iter().filter(|r| r.unused == Some("limit")).map(|r| r.entry.name.as_str()).collect::<Vec<_>>(), [format!("Action {TEAM_MAX_ACTIONS}"), format!("Action {}", TEAM_MAX_ACTIONS + 1)]);
        assert_eq!(db.list_snippets().unwrap().len(), TEAM_MAX_SNIPPETS);
        assert!(db.team_action(&format!("action {TEAM_MAX_ACTIONS}")).unwrap().is_none(), "past the limit");
        assert_eq!(status(&db).unwrap().skipped, 3);
    }

    #[test]
    fn the_users_own_entries_win() {
        let db = temp_db();
        own(&db, "Kubernetes");
        db.upsert_snippet(Snippet { id: None, trigger: "Sig".into(), expansion: "mine".into(), created_at: None }).unwrap();
        sync_with(&db, page(Some(("t1", "Acme")), 5, vec![
            word("e1", " kubernetes ", Some("K8s")),
            snippet("e2", "sig", "team"),
            action("e3", "polish", "replace"),
            action("e4", "Team only", "copy"),
        ], false, false));
        let shadowed = db.list_dictionary_rows().unwrap().into_iter().find(|r| r.source == "team").unwrap();
        assert_eq!(shadowed.unused, Some("shadowed"), "kept but not used");
        assert_eq!(db.list_dictionary().unwrap().len(), 1);
        assert!(db.vocabulary(100, 0, |_| false, &[]).unwrap() == ["Kubernetes"]);
        assert_eq!(db.list_snippets().unwrap().iter().map(|s| s.expansion.as_str()).collect::<Vec<_>>(), ["mine"]);
        assert!(db.team_action("polish").unwrap().is_none(), "a built-in name wins");
        assert!(db.team_action("team only").unwrap().is_some());
        assert!(db.team_action("please team only").unwrap().is_none(), "only its exact name");
        assert_eq!(status(&db).unwrap().skipped, 0, "shadowed entries are shown, not counted as skipped");
        // Deleting the user's own makes the team's used.
        let id = db.list_dictionary().unwrap()[0].id.unwrap();
        db.delete_dictionary(id).unwrap();
        assert_eq!(db.list_dictionary().unwrap()[0].replacement.as_deref(), Some("K8s"));
    }

    #[test]
    fn the_users_own_come_first_and_shadow_as_the_matchers_compare() {
        let db = temp_db();
        own(&db, "post gress");
        db.upsert_dictionary(DictEntry { id: None, phrase: "kube".into(), replacement: Some("Kubernetes".into()), created_at: None, learned: false }).unwrap();
        db.upsert_snippet(Snippet { id: None, trigger: "my sig".into(), expansion: "mine".into(), created_at: None }).unwrap();
        sync_with(&db, page(Some(("t1", "Acme")), 5, vec![
            word("e1", "post-gress", Some("PostgreSQL DB")),
            word("e2", "KUBERNETES", Some("K8s")),
            word("e3", "Terraform", None),
            snippet("e4", "my  sig", "team"),
            snippet("e5", "my sig.", "team"),
            snippet("e6", "other", "team"),
        ], false, false));
        let unused = |p: &str| db.list_dictionary_rows().unwrap().into_iter().find(|r| r.source == "team" && r.entry.phrase == p).unwrap().unused;
        assert_eq!((unused("post-gress"), unused("KUBERNETES"), unused("Terraform")), (Some("shadowed"), Some("shadowed"), None), "spaces and hyphens, and the user's outputs");
        let snippets: Vec<_> = db.list_snippet_rows().unwrap().into_iter().map(|r| (r.entry.trigger, r.unused)).collect();
        assert_eq!(snippets, [("my sig".into(), None), ("other".into(), None), ("my sig.".into(), Some("shadowed")), ("my  sig".into(), Some("shadowed"))]);
        // The user's own first (newest first), so they win wherever the first match counts.
        let dict: Vec<_> = db.list_dictionary().unwrap().into_iter().map(|e| e.phrase).collect();
        assert_eq!(dict, ["kube", "post gress", "Terraform"]);
        own(&db, "zzz newest own");
        assert_eq!(db.list_dictionary().unwrap()[0].phrase, "zzz newest own");
        assert_eq!(db.list_snippets().unwrap().iter().map(|s| s.trigger.as_str()).collect::<Vec<_>>(), ["my sig", "other"]);
    }

    #[test]
    fn a_team_dictionary_that_breaks_the_matcher_leaves_the_users_own_working() {
        let db = temp_db();
        own(&db, "Kubernetes");
        // Many one-letter words with wide case folds ("k" also matches the Kelvin sign), within the byte budget.
        let entries: Vec<Value> = (0..1900).map(|i| word(&format!("e{i}"), &format!("{} {i}", "k ſ ".repeat(23)), None)).collect();
        sync_with(&db, page(Some(("t1", "Acme")), 1, entries, false, false));
        assert!(!crate::text::dictionary_builds(&db.list_dictionary_rows().unwrap().into_iter().map(|r| r.entry).collect::<Vec<_>>()), "the combined set is pathological");
        let used = db.list_dictionary().unwrap();
        assert_eq!(used.iter().map(|e| e.phrase.as_str()).collect::<Vec<_>>(), ["Kubernetes"]);
        assert_eq!(crate::text::apply_dictionary("deploy to kubernetes", &used), "deploy to Kubernetes");
        let st = status(&db).unwrap();
        assert_eq!(st.skipped, 1900);
        assert!(st.error.unwrap().contains("too complex"));
    }

    #[test]
    fn a_hostile_server_cant_grow_the_library_past_a_teams_limit() {
        let db = temp_db();
        let many = |from: usize, n: usize| (from..from + n).map(|i| word(&format!("e{i}"), &format!("w{i}"), None)).collect::<Vec<_>>();
        let err = apply(&db, &cursor(&db).unwrap(), &page(Some(("t1", "Acme")), 1, many(0, MAX_ENTRIES + 1), false, false)).unwrap_err();
        assert!(err.to_string().contains("more than 2000"), "{err}");
        assert!(team_words(&db).is_empty() && cursor(&db).unwrap().version == 0, "nothing applied");
        sync_with(&db, page(Some(("t1", "Acme")), 2000, many(0, 2000), false, false));
        // Incremental pages of new ids: refused once the total passes the margin, and rolled back.
        sync_with(&db, page(Some(("t1", "Acme")), 2050, many(2000, 50), true, false));
        let err = apply(&db, &cursor(&db).unwrap(), &page(Some(("t1", "Acme")), 2200, many(2050, 150), true, false)).unwrap_err();
        assert!(err.to_string().contains("more team library entries than a team can have"), "{err}");
        assert_eq!((team_words(&db).len(), cursor(&db).unwrap().version), (2050, 2050));
        // Skipped ids count too, so they can't pile up either.
        let bad: Vec<Value> = (0..100).map(|i| word(&format!("bad{i}"), "a\rb", None)).collect();
        assert!(apply(&db, &cursor(&db).unwrap(), &page(Some(("t1", "Acme")), 2300, bad, false, false)).is_err());
    }

    /// Dictation reads the dictionary (with team entries) a few times: cargo test --release team_dictionary_speed -- --ignored --nocapture
    #[test]
    #[ignore]
    fn team_dictionary_speed() {
        let db = temp_db();
        for i in 0..1000 {
            own(&db, &format!("own word {i}"));
        }
        let entries = (0..2000).map(|i| word(&format!("e{i}"), &format!("team word {i}"), Some("Team Word"))).collect();
        sync_with(&db, page(Some(("t1", "Acme")), 1, entries, false, false));
        db.list_dictionary().unwrap();
        let t = std::time::Instant::now();
        for _ in 0..20 {
            std::hint::black_box(db.list_dictionary().unwrap());
        }
        println!("1000 own + 2000 team entries: list_dictionary {:?}", t.elapsed() / 20);
    }

    #[test]
    fn team_entries_are_read_only_here() {
        let db = temp_db();
        sync_with(&db, page(Some(("t1", "Acme")), 3, vec![word("e1", "Kubernetes", None), snippet("e2", "sig", "team"), action("e3", "Legalese", "replace")], false, false));
        let word_id = db.list_dictionary().unwrap()[0].id;
        let snippet_id = db.list_snippets().unwrap()[0].id;
        let action_id = db.list_transform_rows().unwrap().into_iter().find(|r| r.team).unwrap().entry.id;
        let refused = |r: Result<()>| assert_eq!(r.unwrap_err().to_string(), crate::db::TEAM_READ_ONLY);
        refused(db.upsert_dictionary(DictEntry { id: word_id, phrase: "x".into(), replacement: None, created_at: None, learned: false }).map(|_| ()));
        refused(db.delete_dictionary(word_id.unwrap()));
        refused(db.upsert_snippet(Snippet { id: snippet_id, trigger: "x".into(), expansion: "y".into(), created_at: None }).map(|_| ()));
        refused(db.delete_snippet(snippet_id.unwrap()));
        refused(db.upsert_transform(Transform { id: action_id, name: "x".into(), prompt: "y".into(), builtin: false, output: "replace".into() }).map(|_| ()));
        refused(db.delete_transform(action_id.unwrap()));
        let st = status(&db).unwrap();
        assert_eq!((st.dictionary, st.snippets, st.actions), (1, 1, 1));

        // Auto-learn neither changes nor deletes them; the user's learned entry then wins.
        use crate::learn::Correction;
        let learned = db.learn(vec![Correction { phrase: "Kubernetes".into(), replacement: Some("K8s".into()) }]).unwrap();
        assert!(learned.undo.iter().all(|u| Some(u.id) != word_id));
        assert_eq!(team_words(&db), vec![("Kubernetes".into(), None)]);
        // Undo rows naming a team entry (they come from the page) are refused too.
        db.undo_learn(&[crate::db::UndoRow { id: word_id.unwrap(), before: None, after: Some(("Kubernetes".into(), None)) }]).unwrap();
        assert_eq!(team_words(&db).len(), 1);
    }

    #[test]
    fn team_actions_are_never_automatic() {
        let db = temp_db();
        sync_with(&db, page(Some(("t1", "Acme")), 1, vec![action("e1", "Legalese", "replace")], false, false));
        assert!(!db.list_transforms().unwrap().iter().any(|t| t.name == "Legalese"), "auto actions, backups and lookups by name use list_transforms");
        assert!(db.team_action("legalese").unwrap().is_some());
    }

    #[test]
    fn forget_clears_and_drops_answers_in_flight() {
        let db = temp_db();
        sync_with(&db, page(Some(("t1", "Acme")), 2, vec![word("e1", "one", None), snippet("e2", "sig", "x"), action("e3", "Act", "copy"), word("bad", "a\rb", None)], false, false));
        let in_flight = cursor(&db).unwrap();
        assert!(forget(&db).unwrap());
        let st = status(&db).unwrap();
        assert_eq!(st, TeamStatus { team: None, dictionary: 0, snippets: 0, actions: 0, skipped: 0, synced_at: None, error: None });
        // Signed out (or the setting turned off) while a request was out: its answer is dropped.
        let a = apply(&db, &in_flight, &page(Some(("t1", "Acme")), 3, vec![word("e4", "late", None)], false, false)).unwrap();
        assert_eq!(a, Applied { step: Step::Stale, changed: false });
        assert!(team_words(&db).is_empty());
        assert!(!forget(&db).unwrap(), "nothing left");
        // A first sync in flight across a sign-out too (the cursor looked the same before and after).
        let first = cursor(&db).unwrap();
        forget(&db).unwrap();
        assert_eq!(apply(&db, &first, &page(Some(("t1", "Acme")), 1, vec![word("e1", "one", None)], false, false)).unwrap().step, Step::Stale);
    }

    #[test]
    fn migrates_a_database_without_team_columns() {
        let dir = std::env::temp_dir().join(format!("opstrellis-team-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE dictionary (id INTEGER PRIMARY KEY, phrase TEXT NOT NULL, replacement TEXT, created_at TEXT NOT NULL, source TEXT NOT NULL DEFAULT 'manual', hits INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE snippets (id INTEGER PRIMARY KEY, trigger TEXT NOT NULL, expansion TEXT NOT NULL, created_at TEXT NOT NULL);
                 CREATE TABLE transforms (id INTEGER PRIMARY KEY, name TEXT NOT NULL, prompt TEXT NOT NULL, builtin INTEGER NOT NULL DEFAULT 0, output TEXT NOT NULL DEFAULT 'replace');
                 INSERT INTO dictionary (phrase, created_at) VALUES ('Mine', 'x');
                 INSERT INTO snippets (trigger, expansion, created_at) VALUES ('sig', 'mine', 'x');
                 INSERT INTO transforms (name, prompt) VALUES ('Mine', 'p');
                 PRAGMA user_version = 4;",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.list_dictionary().unwrap().len(), 1);
        assert_eq!(db.list_snippet_rows().unwrap().iter().map(|r| r.team).collect::<Vec<_>>(), [false]);
        assert!(db.list_transforms().unwrap().iter().any(|t| t.name == "Mine"));
        assert_eq!(cursor(&db).unwrap(), Cursor::default());
        sync_with(&db, page(Some(("t1", "Acme")), 1, vec![word("e1", "Theirs", None)], false, false));
        drop(db);
        let db = Db::open(&path).unwrap();
        assert_eq!(team_words(&db), vec![("Theirs".into(), None)], "reopening keeps the synced entries");
        assert_eq!(cursor(&db).unwrap().version, 1);
    }

    #[test]
    fn the_setting_is_on_unless_turned_off() {
        let old: crate::settings::Settings = serde_json::from_str(r#"{"cloud":{"baseUrl":"https://cloud.example.com","email":null}}"#).unwrap();
        assert!(old.cloud.team_library, "settings saved before it existed");
        let off: crate::settings::Settings = serde_json::from_str(r#"{"cloud":{"baseUrl":"https://cloud.example.com","teamLibrary":false}}"#).unwrap();
        assert!(!off.cloud.team_library);
    }

    #[test]
    fn backoff() {
        let other = || Err(Failure::Other(anyhow!("offline")));
        assert_eq!(next_wait(INTERVAL, &Ok(())), INTERVAL);
        assert_eq!(next_wait(INTERVAL, &other()), INTERVAL * 2);
        assert_eq!(next_wait(MAX_WAIT, &other()), MAX_WAIT);
        assert_eq!(next_wait(INTERVAL, &Err(Failure::RateLimited)), RATE_LIMITED_WAIT);
        assert_eq!(next_wait(INTERVAL * 8, &Ok(())), INTERVAL);
    }

    /// Pages `apply` until the cursor is current, as `sync` does; returns the number of requests.
    async fn live_run(db: &Db, base: &str, token: &str) -> usize {
        let mut requests = 0;
        loop {
            let sent = cursor(db).unwrap();
            let page = fetch(base, token, sent.version).await.unwrap_or_else(|e| panic!("{e:?}"));
            requests += 1;
            if apply(db, &sent, &page).unwrap().step != Step::Again {
                return requests;
            }
        }
    }

    async fn live_call(base: &str, cookie: &str, method: reqwest::Method, path: &str, body: Value) -> Value {
        let resp = HTTP.request(method, format!("{base}{path}")).header("Cookie", cookie).header("Origin", base).json(&body).send().await.unwrap();
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or_default();
        assert!(status.is_success(), "{path}: {status} {v}");
        v
    }

    /// Against a running server (`cargo test -- --ignored team_live`): `OPSTRELLIS_TEAM_URL`, a team
    /// member's API token in `OPSTRELLIS_TEAM_TOKEN`, and the session cookies of the team's admin
    /// (`OPSTRELLIS_TEAM_ADMIN_COOKIE`, to edit the library) and of that member (`OPSTRELLIS_TEAM_MEMBER_COOKIE`, to leave).
    #[tokio::test]
    #[ignore]
    async fn team_live() {
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
        let (base, token) = (env("OPSTRELLIS_TEAM_URL"), env("OPSTRELLIS_TEAM_TOKEN"));
        let (admin, member) = (env("OPSTRELLIS_TEAM_ADMIN_COOKIE"), env("OPSTRELLIS_TEAM_MEMBER_COOKIE"));
        crate::endpoint::check(&base).unwrap();
        let db = temp_db();

        let requests = live_run(&db, &base, &token).await;
        let first = status(&db).unwrap();
        println!("first sync, {requests} request(s): {first:?}");
        println!("  words {:?}", team_words(&db));
        println!("  snippets {:?}", db.list_snippet_rows().unwrap().iter().filter(|r| r.team).map(|r| (&r.entry.trigger, r.unused)).collect::<Vec<_>>());
        println!("  actions {:?}", db.list_transform_rows().unwrap().iter().filter(|r| r.team).map(|r| (&r.entry.name, &r.entry.output, r.unused)).collect::<Vec<_>>());
        assert!(first.team.is_some() && first.dictionary > 0 && first.snippets > 0 && first.actions > 0);

        let added = live_call(&base, &admin, reqwest::Method::POST, "/api/team/library", json!({ "kind": "dictionary", "data": { "phrase": "Zanzibar", "replacement": "ZNZ" } })).await;
        let id = added["id"].as_str().unwrap().to_string();
        let before = cursor(&db).unwrap().version;
        live_run(&db, &base, &token).await;
        let after = cursor(&db).unwrap().version;
        println!("incremental: version {before} -> {after}, words {:?}", team_words(&db));
        assert!(after > before && team_words(&db).contains(&("Zanzibar".into(), Some("ZNZ".into()))));

        live_call(&base, &admin, reqwest::Method::DELETE, &format!("/api/team/library/{id}"), json!({})).await;
        live_run(&db, &base, &token).await;
        println!("after the admin deleted it: version {}, words {:?}", cursor(&db).unwrap().version, team_words(&db));
        assert!(!team_words(&db).iter().any(|w| w.0 == "Zanzibar"));

        live_call(&base, &member, reqwest::Method::POST, "/api/team/leave", json!({})).await;
        live_run(&db, &base, &token).await;
        let left = status(&db).unwrap();
        println!("after leaving the team: {left:?}");
        assert!(left.team.is_none() && left.dictionary + left.snippets + left.actions == 0);

        let revoked = fetch(&base, "utt_not-a-real-token", 0).await;
        println!("with an unknown token: {revoked:?}");
        assert!(matches!(revoked, Err(Failure::SignedOut)));
    }
}
