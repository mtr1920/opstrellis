import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useEffect, useRef, useState } from "react";
import { api, errMsg, isTauri, useEvent, type Workspace } from "../api";
import { Icon } from "../icons";
import { Row, Section, Toggle, fmtNum, type SettingsProps } from "../ui";

function ago(iso: string, now: number) {
  const min = Math.floor((now - new Date(iso).getTime()) / 60e3);
  if (min < 1) return "just now";
  if (min < 60) return `${min} min ago`;
  const h = Math.floor(min / 60);
  return h < 24 ? `${h} h ago` : `${Math.floor(h / 24)} d ago`;
}

function status(w: Workspace, now: number) {
  if (w.indexing && !w.indexedAt) return "Indexing…";
  const counts = `${fmtNum(w.files)} files · ${fmtNum(w.identifiers)} names`;
  return w.indexing ? `${counts} · indexing…` : `${counts} · indexed ${w.indexedAt ? ago(w.indexedAt, now) : "never"}`;
}

/** Settings → Workspaces: project folders whose file and code names dictation uses. */
export function WorkspacesSection({ id, settings, set }: SettingsProps & { id: string }) {
  const [list, setList] = useState<Workspace[]>([]);
  const [path, setPath] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [over, setOver] = useState(false);
  const [now, setNow] = useState(Date.now());
  const card = useRef<HTMLDivElement>(null);

  const load = () => api.listWorkspaces().then(setList, (e) => setError(errMsg(e)));
  useEffect(() => void load(), []);
  useEvent("workspaces-changed", () => void load());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 30e3);
    return () => clearInterval(t);
  }, []);

  async function run(action: () => Promise<Workspace[]>) {
    setError(null);
    try {
      setList(await action());
      return true;
    } catch (e) {
      setError(errMsg(e));
      return false;
    }
  }

  async function add(p: string) {
    setBusy(true);
    if (await run(() => api.addWorkspace(p))) setPath("");
    setBusy(false);
  }

  // Folders dropped on the card. The webview reports drops for the whole window, in physical pixels.
  useEffect(() => {
    if (!isTauri) return;
    let unlisten: (() => void) | undefined;
    let gone = false;
    const inside = (pos: { x: number; y: number }) => {
      const r = card.current?.getBoundingClientRect();
      const [x, y] = [pos.x / devicePixelRatio, pos.y / devicePixelRatio];
      return !!r && x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;
    };
    getCurrentWebview()
      .onDragDropEvent(async ({ payload }) => {
        if (payload.type === "enter" || payload.type === "over") setOver(inside(payload.position));
        else if (payload.type === "drop") {
          setOver(false);
          if (!inside(payload.position)) return;
          for (const p of payload.paths) await add(p);
        } else setOver(false);
      })
      .then((u) => (gone ? u() : (unlisten = u)));
    return () => {
      gone = true;
      unlisten?.();
    };
  }, []);

  return (
    <Section id={id} title="Workspaces"
      desc="In code editors, terminals and AI chats, the file and code names in these folders help OpsTrellis hear them, AI polish spell them where you mean them, and “at sign main.rs” become a file mention. Only names are used, never file contents; with a cloud transcription or AI provider, names are sent to it like your dictionary.">
      <div ref={card} className={"card" + (over ? " drop-over" : "")}>
        {list.map((w) => (
          <div key={w.path} className="row">
            <div className="row-text">
              <div className="row-title">{w.name}</div>
              <div className="row-desc ws-path">{w.path}</div>
              <div className={"row-desc" + (w.error ? " error" : "")}>{w.error ?? status(w, now)}</div>
            </div>
            <div className="row-control">
              <button className="btn" disabled={w.indexing} onClick={() => run(() => api.reindexWorkspace(w.path))}>
                {w.indexing ? "Indexing…" : "Refresh"}
              </button>
              <button className="icon-btn" title="Remove" aria-label={`Remove ${w.name}`}
                onClick={() => run(() => api.removeWorkspace(w.path))}><Icon name="trash" /></button>
            </div>
          </div>
        ))}
        <Row title="Add a folder" desc={(isTauri ? "Paste its full path, or drop the folder here." : "Paste its full path.") + " Respects simple .gitignore rules."}>
          <form className="inline" onSubmit={(e) => { e.preventDefault(); if (path.trim()) void add(path); }}>
            <input className="wide" value={path} onChange={(e) => setPath(e.target.value)} placeholder="/Users/you/code/project"
              aria-label="Folder path" spellCheck={false} autoCapitalize="off" autoCorrect="off" />
            <button className="btn" type="submit" disabled={busy || !path.trim()}>Add</button>
          </form>
        </Row>
        {error && <div className="error">{error}</div>}
        <Row title="Write spoken names as identifiers, without AI"
          desc="In editors and terminals, “user session store” becomes UserSessionStore without AI polish. It can't tell a name from an ordinary phrase, so it can rewrite your words too: “plain text” becomes plain_text if your code has that name. For people who dictate code.">
          <Toggle checked={settings.workspaceReplace} onChange={(v) => set("workspaceReplace", v)} label="Write spoken names as identifiers, without AI" />
        </Row>
        <div className="list-empty">Try it in an AI chat: say “at sign pipeline dot rs”, or, with AI polish on, a name from your code such as “user session store”.</div>
      </div>
    </Section>
  );
}
