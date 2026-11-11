import { useEffect, useState } from "react";
import { api, errMsg, useEvent, type Snippet } from "../api";
import { Icon } from "../icons";
import { Empty, Modal, Page, Search, useSearch } from "../ui";

type Draft = { id?: number; trigger: string; expansion: string };

/** Placeholders filled in when the snippet expands (text.rs `expand_vars`). */
const VARS: [string, string][] = [
  ["{date}", "today's date"], ["{weekday}", "the day of the week"], ["{time}", "the current time"], ["{clipboard}", "the text on your clipboard"],
];

export default function Snippets() {
  const [snippets, setSnippets] = useState<Snippet[]>([]);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [error, setError] = useState<string | null>(null);
  const { query, setQuery, filtered } = useSearch(snippets, (s) => `${s.trigger} ${s.expansion}`);

  const load = () => api.listSnippets().then(setSnippets, (e) => setError(errMsg(e)));
  useEffect(() => void load(), []);
  useEvent("team-updated", () => void load());

  async function save() {
    if (!draft || !draft.trigger.trim() || !draft.expansion.trim()) return;
    try {
      await api.upsertSnippet({ id: draft.id, trigger: draft.trigger.trim(), expansion: draft.expansion });
      setDraft(null);
      load();
    } catch (e) { setError(errMsg(e)); }
  }
  async function remove(id: number) {
    await api.deleteSnippet(id).catch((e) => setError(errMsg(e)));
    load();
  }
  const add = () => setDraft({ trigger: "", expansion: "" });

  return (
    <Page title="Snippets" actions={<>
      {snippets.length > 0 && <Search value={query} onChange={setQuery} />}
      <button className="btn primary" onClick={add}><Icon name="plus" />Add new</button>
    </>}>
      {error && <div className="error">{error}</div>}

      {snippets.length === 0 ? (
        <Empty action={<button className="btn" onClick={add}>Create a snippet</button>}>
          Give text you type often a short spoken trigger.
        </Empty>
      ) : (
        <div className="card list">
          {filtered.map((s) => (
            <div key={s.id} className="list-row">
              <div className="li-main">
                <div className="li-title">“{s.trigger}”{s.team && <span className="tag" title="From your team's library. Edit it in the team dashboard.">Team</span>}</div>
                {s.unused && <div className="li-meta">{s.unused === "shadowed" ? "Yours is used" : "Not used: over the team snippet limit"}</div>}
                <div className="li-meta clamp">{s.expansion}</div>
              </div>
              {!s.team && (
                <div className="li-actions">
                  <button className="icon-btn" title="Edit" aria-label="Edit" onClick={() => setDraft({ id: s.id, trigger: s.trigger, expansion: s.expansion })}><Icon name="edit" /></button>
                  <button className="icon-btn" title="Delete" aria-label="Delete" onClick={() => remove(s.id)}><Icon name="trash" /></button>
                </div>
              )}
            </div>
          ))}
          {filtered.length === 0 && <div className="list-empty">No snippets match “{query}”.</div>}
        </div>
      )}

      {draft && (
        <Modal title={draft.id ? "Edit snippet" : "New snippet"} onClose={() => setDraft(null)}
          footer={<>
            <button className="btn" onClick={() => setDraft(null)}>Cancel</button>
            <button className="btn primary" onClick={save} disabled={!draft.trigger.trim() || !draft.expansion.trim()}>Save</button>
          </>}>
          <label className="field">
            <span>Spoken trigger</span>
            <input autoFocus value={draft.trigger} onChange={(e) => setDraft({ ...draft, trigger: e.target.value })} placeholder="e.g. my calendar link" />
          </label>
          <label className="field">
            <span>Expands to</span>
            <textarea rows={5} value={draft.expansion} onChange={(e) => setDraft({ ...draft, expansion: e.target.value })} placeholder="The text to insert" />
          </label>
          <div className="var-chips" role="group" aria-label="Insert a placeholder">
            <span className="meta">Insert:</span>
            {VARS.map(([v, label]) => (
              <button key={v} type="button" className="chip" title={`Inserts ${label}`}
                onClick={() => setDraft({ ...draft, expansion: draft.expansion + (draft.expansion && !/\s$/.test(draft.expansion) ? " " : "") + v })}>{v}</button>
            ))}
          </div>
        </Modal>
      )}
    </Page>
  );
}
