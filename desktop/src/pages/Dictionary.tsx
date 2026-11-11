import { useEffect, useRef, useState } from "react";
import { api, errMsg, useEvent, type DictEntry } from "../api";
import { Icon } from "../icons";
import { Empty, Modal, Page, Search, useSearch } from "../ui";

type Draft = { id?: number; phrase: string; replacement: string };

export default function Dictionary() {
  const [entries, setEntries] = useState<DictEntry[]>([]);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const { query, setQuery, filtered } = useSearch(entries, (e) => `${e.phrase} ${e.replacement ?? ""}`);

  const load = () => api.listDictionary().then(setEntries, (e) => setError(errMsg(e)));
  useEffect(() => void load(), []);
  useEvent("team-updated", () => void load());

  async function save() {
    if (!draft || !draft.phrase.trim()) return;
    try {
      await api.upsertDictionary({ id: draft.id, phrase: draft.phrase.trim(), replacement: draft.replacement.trim() || null });
      setDraft(null);
      load();
    } catch (e) { setError(errMsg(e)); }
  }
  async function remove(id: number) {
    await api.deleteDictionary(id).catch((e) => setError(errMsg(e)));
    load();
  }

  const add = () => setDraft({ phrase: "", replacement: "" });

  async function importList(file: File | undefined) {
    if (!file) return;
    setError(null);
    setNotice(null);
    if (file.size > 5 * 1024 * 1024) return setError("That file is over 5 MB.");
    try {
      const r = await api.importDictionaryText(await file.text());
      setNotice(`Added ${r.add}, skipped ${r.skip} already there`);
      load();
    } catch (e) { setError(errMsg(e)); }
  }

  return (
    <Page title="Dictionary" actions={<>
      {entries.length > 0 && <Search value={query} onChange={setQuery} />}
      <button className="btn" onClick={() => fileInput.current?.click()}>Import list…</button>
      <button className="btn primary" onClick={add}><Icon name="plus" />Add new</button>
    </>}>
      <input ref={fileInput} type="file" accept=".txt,text/plain" hidden
        onChange={(e) => { void importList(e.target.files?.[0]); e.target.value = ""; }} />
      {error && <div className="error">{error}</div>}
      {notice && <div className="note">{notice}</div>}

      {entries.length === 0 ? (
        <Empty action={<button className="btn" onClick={add}>Add a word</button>}>
          Add names and jargon OpsTrellis should spell right, or phrases to replace.
        </Empty>
      ) : (
        <div className="card list">
          {filtered.map((e) => (
            <div key={e.id} className="list-row">
              <div className="li-main">
                <div className="li-title">{e.phrase}</div>
                <div className="li-meta">
                  {e.replacement ? <>Replaced with <span className="li-em">{e.replacement}</span></> : "Spelling"}
                  {e.learned && <span className="tag">Learned</span>}
                  {e.source === "agent" && <span className="tag" title="Added by an AI agent">Agent</span>}
                  {e.source === "team" && <span className="tag" title="From your team's library. Edit it in the team dashboard.">Team</span>}
                  {e.unused && <> · {e.unused === "shadowed" ? "yours is used" : e.unused === "complex" ? "not used: too complex to match" : "not used: the dictionary is full"}</>}
                </div>
              </div>
              {e.source !== "team" && (
                <div className="li-actions">
                  <button className="icon-btn" title="Edit" aria-label="Edit" onClick={() => setDraft({ id: e.id, phrase: e.phrase, replacement: e.replacement ?? "" })}><Icon name="edit" /></button>
                  <button className="icon-btn" title="Delete" aria-label="Delete" onClick={() => remove(e.id)}><Icon name="trash" /></button>
                </div>
              )}
            </div>
          ))}
          {filtered.length === 0 && <div className="list-empty">No entries match “{query}”.</div>}
        </div>
      )}
      <div className="meta">Import list: a text file with one entry per line. Write a word, or a word and its replacement with a tab, a comma or “ =&gt; ”, “ -&gt; ” or “ → ” (with spaces) between them. Lines starting with # are ignored.</div>

      {draft && (
        <Modal title={draft.id ? "Edit entry" : "Add to dictionary"} onClose={() => setDraft(null)}
          footer={<>
            <button className="btn" onClick={() => setDraft(null)}>Cancel</button>
            <button className="btn primary" onClick={save} disabled={!draft.phrase.trim()}>Save</button>
          </>}>
          <label className="field">
            <span>Word or spoken phrase</span>
            <input autoFocus value={draft.phrase} onChange={(e) => setDraft({ ...draft, phrase: e.target.value })}
              onKeyDown={(e) => e.key === "Enter" && save()} placeholder="e.g. Kubernetes" />
          </label>
          <label className="field">
            <span>Replace with (optional)</span>
            <input value={draft.replacement} onChange={(e) => setDraft({ ...draft, replacement: e.target.value })}
              onKeyDown={(e) => e.key === "Enter" && save()} placeholder="Leave empty to use it as a vocabulary hint" />
          </label>
        </Modal>
      )}
    </Page>
  );
}
