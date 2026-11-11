import { useEffect, useState } from "react";
import { api, errMsg, useEvent, type Transform } from "../api";
import { Icon } from "../icons";
import { Keys, Modal, Page, type SettingsProps } from "../ui";

type Draft = { id?: number; name: string; prompt: string; output: Transform["output"] };

export default function Transforms({ settings, set }: SettingsProps) {
  const [items, setItems] = useState<Transform[]>([]);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = () => api.listTransforms().then(setItems, (e) => setError(errMsg(e)));
  useEffect(() => void load(), []);
  useEvent("team-updated", () => void load());

  async function save() {
    if (!draft || !draft.name.trim() || !draft.prompt.trim()) return;
    try {
      await api.upsertTransform({ id: draft.id, name: draft.name.trim(), prompt: draft.prompt.trim(), output: draft.output });
      setDraft(null);
      load();
    } catch (e) { setError(errMsg(e)); }
  }
  async function remove(id: number) {
    try {
      await api.deleteTransform(id);
      // SQLite can reuse a deleted id, so a stale auto transform would silently run a new transform.
      const auto = Object.entries(settings.autoTransforms).filter(([, v]) => v !== id);
      if (auto.length !== Object.keys(settings.autoTransforms).length) set("autoTransforms", Object.fromEntries(auto));
    } catch (e) { setError(errMsg(e)); }
    load();
  }

  return (
    <Page title="Actions" actions={
      <button className="btn primary" onClick={() => setDraft({ name: "", prompt: "", output: "replace" })}><Icon name="plus" />New action</button>
    }>
      <div className="card hint">
        <span className="hint-main">Select text, hold <Keys combo={settings.hotkeys.command} /> and say what to change, or the name of an action below.</span>
      </div>
      {!settings.llm.enabled && <p className="note">Command mode needs AI polish. Turn it on in Settings → AI polish.</p>}
      {error && <div className="error">{error}</div>}

      <div className="card list">
        {items.map((t) => (
          <div key={t.id} className="list-row">
            <div className="li-main">
              <div className="li-title">{t.name}{t.builtin && <span className="tag">Built-in</span>}{t.team && <span className="tag" title="From your team's library. Edit it in the team dashboard. Runs when you say exactly its name in command mode; never an auto action.">Team</span>}{t.output === "copy" && <span className="tag">Copies result</span>}</div>
              {t.unused && <div className="li-meta">{t.unused === "shadowed" ? "Yours is used" : "Not used: over the team action limit"}</div>}
              <div className="li-meta clamp">{t.prompt}</div>
            </div>
            {!t.team && (
              <div className="li-actions">
                <button className="icon-btn" title="Edit" aria-label="Edit" onClick={() => setDraft({ id: t.id, name: t.name, prompt: t.prompt, output: t.output })}><Icon name="edit" /></button>
                {!t.builtin && <button className="icon-btn" title="Delete" aria-label="Delete" onClick={() => remove(t.id)}><Icon name="trash" /></button>}
              </div>
            )}
          </div>
        ))}
        {items.length === 0 && <div className="list-empty">No actions yet.</div>}
      </div>

      {draft && (
        <Modal title={draft.id ? "Edit action" : "New action"} onClose={() => setDraft(null)}
          footer={<>
            <button className="btn" onClick={() => setDraft(null)}>Cancel</button>
            <button className="btn primary" onClick={save} disabled={!draft.name.trim() || !draft.prompt.trim()}>Save</button>
          </>}>
          <label className="field">
            <span>Name <em>say this in command mode</em></span>
            <input autoFocus value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} placeholder="e.g. Make it shorter" />
          </label>
          <label className="field">
            <span>Instruction</span>
            <textarea rows={5} value={draft.prompt} onChange={(e) => setDraft({ ...draft, prompt: e.target.value })}
              placeholder="Rewrite the text to be half as long, keeping the key points." />
          </label>
          <label className="field">
            <span>Result <em>in command mode</em></span>
            <select value={draft.output} onChange={(e) => setDraft({ ...draft, output: e.target.value as Transform["output"] })}>
              <option value="replace">Replace the selected text</option>
              <option value="copy">Copy to the clipboard (e.g. a reply to paste elsewhere)</option>
            </select>
          </label>
        </Modal>
      )}
    </Page>
  );
}
