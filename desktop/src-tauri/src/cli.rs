//! Subcommands answered before the app starts (no window, tray or single instance): the MCP server
//! and the agent-access commands, which read the user's data as `agent.rs` allows.

use crate::agent::{self, Store, Tool};
use serde_json::{json, Value};
use std::io::Write;

pub const HELP: &str = "\
OpsTrellis: voice dictation for every app.

Usage:
  opstrellis                    Open OpsTrellis
  opstrellis last [--json]      Print your last dictation
  opstrellis history [--search <text>] [--limit <1-50>] [--json]
                                Print recent dictations, newest first
  opstrellis notes [--kind note|meeting] [--json]
                                List notes and meeting notes
  opstrellis note <id> [--json] Print one note
  opstrellis ask \"<question>\" [--json]
                                Show a question in the pill and print your spoken answer
  opstrellis mcp                Run the MCP server for AI agents (stdio)

Flags for the running app, e.g. for desktop keyboard shortcuts:
  --toggle       Start or stop dictation
  --command      Start or stop command mode
  --paste-last   Paste the last transcript
  --cancel       Cancel the current dictation
  -V, --version  Print the version
  -h, --help     Print this help

last, history, notes, note, ask and mcp need agent access, in OpsTrellis → Settings → Agents.
";

/// The app's own flags (`lib.rs`), which start or reach the app as before.
const APP_FLAGS: [&str; 7] = ["--background", "--toggle", "--command", "--paste-last", "--cancel", "--version", "-V"];
const SUBCOMMANDS: [&str; 7] = ["mcp", "last", "history", "notes", "note", "ask", "help"];

#[derive(Debug, PartialEq)]
enum Command {
    App,
    Help,
    Mcp,
    Last { json: bool },
    History { search: Option<String>, limit: Option<i64>, json: bool },
    Notes { kind: Option<String>, json: bool },
    Note { id: i64, json: bool },
    Ask { question: String, json: bool },
    /// Hidden, for release checks: prints whether this copy can install updates itself.
    CanSelfUpdate,
}

/// Runs a subcommand and returns its exit code, or None to start the app (no arguments, or the
/// app's own flags, which keep working as before).
pub fn dispatch(args: &[String]) -> Option<i32> {
    match parse(args) {
        Ok(Command::App) => None,
        Ok(Command::Help) => {
            let _ = std::io::stdout().write_all(HELP.as_bytes());
            Some(0)
        }
        Ok(Command::Mcp) => Some(crate::mcp::serve()),
        Ok(Command::CanSelfUpdate) => {
            println!("{}", if crate::update::self_update_supported() { "yes" } else { "no" });
            Some(0)
        }
        Ok(cmd) => Some(run(&Store::locate(), cmd, &mut std::io::stdout().lock(), &mut std::io::stderr().lock())),
        Err(e) => {
            let _ = write!(std::io::stderr(), "opstrellis: {e}\n\n{HELP}");
            Some(2)
        }
    }
}

fn parse(args: &[String]) -> Result<Command, String> {
    let help = |a: &String| a == "--help" || a == "-h";
    let Some((first, rest)) = args.split_first() else { return Ok(Command::App) };
    if first == "--can-self-update" && rest.is_empty() {
        return Ok(Command::CanSelfUpdate);
    }
    if first.starts_with('-') {
        if args.iter().any(help) {
            return Ok(Command::Help);
        }
        // `--json last`: options go after the command; don't open the app instead of failing.
        let own = APP_FLAGS.contains(&first.as_str()) || first.starts_with("-psn_");
        if !own && rest.iter().any(|a| SUBCOMMANDS.contains(&a.as_str())) {
            return Err(format!("Put {} after the command, e.g. `opstrellis last --json`.", shown(first)));
        }
        return Ok(Command::App);
    }
    if first == "help" || rest.iter().any(help) {
        return Ok(Command::Help);
    }
    let (mut json, mut search, mut limit, mut kind, mut id, mut question) = (false, None, None, None, None, None);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match (first.as_str(), a.as_str()) {
            ("last" | "history" | "notes" | "note" | "ask", "--json") => json = true,
            ("history", "--search") => search = Some(value("--search")?),
            ("history", "--limit") => limit = Some(value("--limit")?.parse::<i64>().map_err(|_| "--limit needs a whole number".to_string())?),
            ("notes", "--kind") => kind = Some(value("--kind")?),
            ("note", n) if id.is_none() && !n.starts_with('-') => id = Some(n.parse::<i64>().map_err(|_| format!("Not a note id: {}", shown(n)))?),
            ("ask", q) if question.is_none() => question = Some(q.to_string()),
            ("mcp" | "last" | "history" | "notes" | "note" | "ask", _) => return Err(format!("Unexpected argument for {first}: {}", shown(a))),
            _ => return Err(format!("Unknown command: {}", shown(first))),
        }
    }
    Ok(match first.as_str() {
        "mcp" => Command::Mcp,
        "last" => Command::Last { json },
        "history" => Command::History { search, limit, json },
        "notes" => Command::Notes { kind, json },
        "note" => Command::Note { id: id.ok_or("note needs an id (see `opstrellis notes`)")?, json },
        "ask" => Command::Ask { question: question.ok_or("ask needs a question, in quotes")?, json },
        _ => return Err(format!("Unknown command: {}", shown(first))),
    })
}

fn shown(s: &str) -> String {
    s.chars().take(64).collect()
}

fn run(store: &Store, cmd: Command, out: &mut impl Write, err: &mut impl Write) -> i32 {
    let (tool, args, json) = match cmd {
        Command::Last { json } => (Tool::GetLastDictation, json!({}), json),
        Command::History { search, limit, json } => (Tool::SearchDictations, json!({ "query": search, "limit": limit }), json),
        Command::Notes { kind, json } => (Tool::ListNotes, json!({ "kind": kind, "limit": agent::MAX_NOTES }), json),
        Command::Note { id, json } => (Tool::GetNote, json!({ "id": id }), json),
        Command::Ask { question, json } => (Tool::AskUser, json!({ "question": question }), json),
        Command::App | Command::Help | Command::Mcp | Command::CanSelfUpdate => return 2,
    };
    let result = match args.as_object().map(|a| agent::call(store, tool, a)) {
        Some(Ok(v)) => v,
        Some(Err(e)) => {
            let _ = writeln!(err, "{e}");
            return 1;
        }
        None => return 2,
    };
    if tool == Tool::GetLastDictation && result["dictation"].is_null() && !json {
        let _ = writeln!(err, "No dictation is saved.");
        return 1;
    }
    let text = if json { serde_json::to_string_pretty(&result).unwrap_or_default() } else { plain(tool, &result) };
    let text = text.trim_end();
    if text.is_empty() {
        return 0;
    }
    match writeln!(out, "{text}").and_then(|()| out.flush()) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}

/// "2026-11-10 14:03" from an RFC 3339 time, in the time zone it was saved with.
fn when(v: &Value, key: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(s(v, key)).map_or_else(|_| s(v, key).to_string(), |t| t.format("%Y-%m-%d %H:%M").to_string())
}

fn plain(tool: Tool, v: &Value) -> String {
    let empty = Vec::new();
    let list = |key: &str| v[key].as_array().unwrap_or(&empty).clone();
    match tool {
        Tool::GetLastDictation => s(&v["dictation"], "text").to_string(),
        Tool::SearchDictations => list("dictations")
            .iter()
            .map(|d| format!("{}  {}\n{}\n", when(d, "createdAt"), d["app"].as_str().unwrap_or("-"), s(d, "text")))
            .collect::<Vec<_>>()
            .join("\n"),
        Tool::ListNotes => list("notes")
            .iter()
            .map(|n| format!("{}\t{}\t{}\t{}\n", n["id"], s(n, "kind"), when(n, "updatedAt"), s(n, "title")))
            .collect(),
        Tool::GetNote => {
            let cut = if v["truncated"] == true { format!("\n\n[Cut at {} characters]", agent::NOTE_BODY_CHARS) } else { String::new() };
            format!("{}\n{} · updated {}\n\n{}{cut}", s(v, "title"), s(v, "kind"), when(v, "updatedAt"), s(v, "body"))
        }
        Tool::AskUser => s(v, "answer").to_string(),
        Tool::ListDictionary | Tool::AddDictionaryTerm => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tests::Fixture;

    fn parsed(args: &[&str]) -> Result<Command, String> {
        parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn the_self_update_check_is_hidden() {
        assert_eq!(parsed(&["--can-self-update"]), Ok(Command::CanSelfUpdate));
        assert_eq!(parsed(&["--can-self-update", "extra"]), Ok(Command::App));
        assert!(!HELP.contains("self-update"));
    }

    #[test]
    fn parses_subcommands_and_leaves_the_app_flags_alone() {
        assert_eq!(parsed(&[]), Ok(Command::App));
        for flags in [&["--toggle"][..], &["--command"], &["--paste-last"], &["--cancel"], &["--background"], &["-psn_0_12345"], &["--toggle", "extra"]] {
            assert_eq!(parsed(flags), Ok(Command::App), "{flags:?}");
        }
        for flags in [&["--background", "last"][..], &["--toggle", "notes"], &["-psn_0_1", "mcp"], &["--cancel", "ask"]] {
            assert_eq!(parsed(flags), Ok(Command::App), "the app's flags keep working: {flags:?}");
        }
        for misplaced in [&["--json", "last"][..], &["--json", "history", "--limit", "3"], &["-x", "mcp"], &["--search", "foo", "history"]] {
            assert!(parsed(misplaced).unwrap_err().contains("after the command"), "{misplaced:?}");
        }
        assert_eq!(parsed(&["--unknown-flag"]), Ok(Command::App), "an unknown flag alone starts the app, as before");
        assert_eq!(parsed(&["--help"]), Ok(Command::Help));
        assert_eq!(parsed(&["-h"]), Ok(Command::Help));
        assert_eq!(parsed(&["--toggle", "--help"]), Ok(Command::Help));
        assert_eq!(parsed(&["help"]), Ok(Command::Help));
        assert_eq!(parsed(&["history", "--help"]), Ok(Command::Help));
        assert_eq!(parsed(&["mcp"]), Ok(Command::Mcp));
        assert_eq!(parsed(&["last"]), Ok(Command::Last { json: false }));
        assert_eq!(parsed(&["last", "--json"]), Ok(Command::Last { json: true }));
        assert_eq!(parsed(&["history"]), Ok(Command::History { search: None, limit: None, json: false }));
        assert_eq!(
            parsed(&["history", "--search", "deploy plan", "--limit", "5", "--json"]),
            Ok(Command::History { search: Some("deploy plan".into()), limit: Some(5), json: true })
        );
        assert_eq!(parsed(&["notes", "--kind", "meeting"]), Ok(Command::Notes { kind: Some("meeting".into()), json: false }));
        assert_eq!(parsed(&["note", "42", "--json"]), Ok(Command::Note { id: 42, json: true }));
        assert_eq!(parsed(&["ask", "Ship it today?"]), Ok(Command::Ask { question: "Ship it today?".into(), json: false }));
        assert_eq!(parsed(&["ask", "--json", "Why?"]), Ok(Command::Ask { question: "Why?".into(), json: true }));
        assert!(parsed(&["ask"]).is_err() && parsed(&["ask", "one", "two"]).is_err());
        for bad in [&["frobnicate"][..], &["mcp", "--stdio"], &["last", "now"], &["history", "--search"], &["history", "--limit", "many"],
                    &["history", "--kind", "note"], &["notes", "--search", "x"], &["note"], &["note", "abc"], &["note", "1", "2"], &["note", "--json"]] {
            assert!(parsed(bad).is_err(), "{bad:?}");
        }
        assert!(HELP.contains("--toggle") && HELP.contains("--paste-last") && HELP.contains("--version") && HELP.contains("opstrellis mcp"));
    }

    fn output(store: &Store, cmd: Command) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(store, cmd, &mut out, &mut err);
        (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
    }

    #[test]
    fn commands_print_what_agents_may_read() {
        let f = Fixture::new();
        let store = f.store();
        f.dictate("deploy at noon", "ok", "dictate", "2026-11-10T14:03:00+01:00");
        f.dictate("broken", "error", "dictate", "2026-11-10T14:04:00+01:00");
        let id = f.note("Standup", "Summary\n\n## Transcript\n\nall good", "meeting");

        let (code, out, err) = output(&store, Command::Last { json: false });
        assert_eq!((code, out.as_str()), (1, ""));
        assert!(err.contains("Settings → Agents"), "off by default: {err}");

        f.agents(true, true, true, false);
        assert_eq!(output(&store, Command::Last { json: false }), (0, "deploy at noon\n".into(), String::new()));
        let (code, out, _) = output(&store, Command::Last { json: true });
        assert_eq!((code, serde_json::from_str::<Value>(&out).unwrap()["dictation"]["text"].as_str()), (0, Some("deploy at noon")));
        assert_eq!(output(&store, Command::History { search: None, limit: None, json: false }).1, "2026-11-10 14:03  Slack\ndeploy at noon\n");
        assert_eq!(output(&store, Command::History { search: Some("nothing".into()), limit: None, json: false }), (0, String::new(), String::new()));
        let (code, _, err) = output(&store, Command::History { search: None, limit: Some(99), json: false });
        assert_eq!(code, 1);
        assert!(err.contains("from 1 to 50"));
        assert!(output(&store, Command::Notes { kind: None, json: false }).1.starts_with(&format!("{id}\tmeeting\t")));
        let (code, out, _) = output(&store, Command::Note { id, json: false });
        assert_eq!(code, 0);
        assert!(out.starts_with("Standup\nmeeting · updated ") && out.ends_with("all good\n"), "{out}");
        assert_eq!(output(&store, Command::Note { id: 999, json: false }).0, 1);

        f.agents(true, false, true, false);
        let (code, _, err) = output(&store, Command::History { search: None, limit: None, json: false });
        assert_eq!(code, 1);
        assert!(err.contains("dictation history"));

        let empty = Fixture::new();
        empty.agents(true, true, true, false);
        assert_eq!(output(&empty.store(), Command::Last { json: false }), (1, String::new(), "No dictation is saved.\n".into()));
    }
}
