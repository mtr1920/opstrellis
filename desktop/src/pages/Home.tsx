import { useEffect, useRef, useState } from "react";
import { api, errMsg, useEvent, type HistoryItem, type Learned, type Stats } from "../api";
import { Icon } from "../icons";
import { Empty, Keys, Page, isSingleKey, Search, Section, dayLabel, fmtNum, fmtTime, type SettingsProps } from "../ui";

export default function Home({ settings, onSetup }: SettingsProps & { onSetup: () => void }) {
  const [stats, setStats] = useState<Stats | null>(null);
  const [items, setItems] = useState<HistoryItem[]>([]);
  const [query, setQuery] = useState("");
  const [secrets, setSecrets] = useState<string[] | null>(null);
  const [copied, setCopied] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<{ id: number; text: string } | null>(null);
  const [learned, setLearned] = useState<Learned | null>(null);
  const [shown, setShown] = useState(PAGE_SIZE);
  const [original, setOriginal] = useState<number | null>(null);
  const [polishing, setPolishing] = useState<number | null>(null);
  const [retrying, setRetrying] = useState<number | null>(null);
  const [retryError, setRetryError] = useState<{ id: number; message: string } | null>(null);
  const request = useRef(0);

  const refresh = () => {
    const id = ++request.current;
    const failed = (e: unknown) => { if (id === request.current) setError(errMsg(e)); };
    api.getStats().then((s) => { if (id === request.current) setStats(s); }, failed);
    // One extra row tells whether there's more to show.
    api.listHistory(query, shown + 1).then((xs) => { if (id === request.current) setItems(xs); }, failed);
  };
  useEffect(refresh, [query, shown]);
  useEffect(() => setShown(PAGE_SIZE), [query]);
  useEffect(() => () => { request.current++; }, []);
  useEffect(() => void api.hasSecrets().then(setSecrets, () => setSecrets([])), []);
  useEvent("history-updated", refresh);

  // A provider counts as configured when it's enabled and has what it needs to run.
  const configured = settings.sttChain.some((e) =>
    e.enabled && (e.provider === "local" || e.provider === "cloud" || secrets?.includes(`stt:${e.id}`)));
  const showSetup = !settings.onboarded || (secrets !== null && !configured);

  async function copy(item: HistoryItem) {
    await navigator.clipboard.writeText(item.text);
    setCopied(item.id);
    setTimeout(() => setCopied((c) => (c === item.id ? null : c)), 1200);
  }
  async function saveEdit() {
    if (!editing) return;
    try {
      const result = await api.updateHistory(editing.id, editing.text);
      setItems((xs) => xs.map((x) => (x.id === editing.id ? { ...x, text: editing.text.trim() } : x)));
      setEditing(null);
      setLearned(result.added.length || result.pending.length || result.full ? result : null);
    } catch (e) { setError(errMsg(e)); }
  }
  async function undoLearned() {
    if (!learned) return;
    await api.undoLearned(learned.undo).catch((e) => setError(errMsg(e)));
    setLearned(null);
  }
  async function repolish(item: HistoryItem) {
    setPolishing(item.id);
    setError(null);
    try {
      const text = await api.repolishHistory(item.id);
      setItems((xs) => xs.map((x) => (x.id === item.id ? { ...x, text } : x)));
    } catch (e) { setError(errMsg(e)); }
    setPolishing(null);
  }
  /** Transcribes the kept recording again; the new text is also put on the clipboard. */
  async function retranscribe(item: HistoryItem) {
    setRetrying(item.id);
    setRetryError(null);
    try {
      const text = await api.retranscribeHistory(item.id);
      setItems((xs) => xs.map((x) => (x.id === item.id ? { ...x, status: "ok", error: null, text } : x)));
    } catch (e) { setRetryError({ id: item.id, message: errMsg(e) }); }
    setRetrying(null);
  }
  async function remove(id: number) {
    await api.deleteHistory(id).catch((e) => setError(errMsg(e)));
    setItems((xs) => xs.filter((x) => x.id !== id));
  }

  const groups: { label: string; items: HistoryItem[] }[] = [];
  for (const item of items.slice(0, shown)) {
    const label = dayLabel(item.createdAt);
    if (groups.at(-1)?.label !== label) groups.push({ label, items: [] });
    groups.at(-1)!.items.push(item);
  }

  return (
    <Page title="Home">
      {showSetup ? (
        <div className="card hint">
          <span className="hint-main">Choose a speech provider to start dictating.</span>
          <button className="btn primary" onClick={onSetup}>Set up OpsTrellis</button>
        </div>
      ) : (
        <div className="card hint">
          <span className="hint-main">Hold <Keys combo={settings.hotkeys.dictate} /> to dictate · {isSingleKey(settings.hotkeys.dictate) ? "double-tap" : "tap"} for hands-free</span>
          <span className="hint-aside">
            <span className="hint-item">Command mode <Keys combo={settings.hotkeys.command} /></span>
            <span className="hint-item">Paste last <Keys combo={settings.hotkeys.pasteLast} /></span>
          </span>
        </div>
      )}

      {stats && (
        <div className="card stat-strip">
          <Stat label="Words dictated" value={fmtNum(stats.totalWords)} />
          <Stat label="Words per minute" value={Math.round(stats.wpm)} />
          <Stat label="Day streak" value={`${stats.streak} ${stats.streak === 1 ? "day" : "days"}`} />
        </div>
      )}

      <Section title="History" aside={<Search value={query} onChange={setQuery} placeholder="Search history" />}>
        {error && <div className="error">{error}</div>}
        {learned && (
          <div className="note learned-note">
            <span>
              {learned.added.length > 0 && <>Added to your dictionary: {learned.added.map(fmtFix).join(", ")}. </>}
              {learned.pending.length > 0 && <>Make the same fix once more to learn {learned.pending.map(fmtFix).join(", ")}. </>}
              {learned.full && <>The dictionary is full, so a fix wasn't learned. Delete some entries to make room.</>}
            </span>
            {learned.added.length > 0 && <button className="link" onClick={undoLearned}>Undo</button>}
            <button className="link" onClick={() => setLearned(null)}>Dismiss</button>
          </div>
        )}
        {groups.length === 0 && (
          <Empty>{query ? "No dictations match your search." : "Your dictations will show up here."}</Empty>
        )}
        {groups.map((g) => (
          <div key={g.label} className="day">
            <h3 className="day-label">{g.label}</h3>
            <div className="card list">
              {g.items.map((item) => (
                <div key={item.id} className={"list-row timed" + (editing?.id === item.id ? " editing" : "")}>
                  <div className="li-time">{fmtTime(item.createdAt)}</div>
                  {editing?.id === item.id ? (
                    <div className="li-main">
                      <textarea className="edit-area" autoFocus value={editing.text} aria-label="Corrected text"
                        onChange={(e) => setEditing({ id: item.id, text: e.target.value })}
                        onKeyDown={(e) => { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) void saveEdit(); if (e.key === "Escape") setEditing(null); }} />
                      <div className="edit-actions">
                        <span className="li-meta">{settings.cleanup.autoLearn ? "Corrected words are added to your dictionary." : "Auto-learn is off (Style)."}</span>
                        <button className="btn" onClick={() => setEditing(null)}>Cancel</button>
                        <button className="btn primary" onClick={saveEdit} disabled={!editing.text.trim()}>Save correction</button>
                      </div>
                    </div>
                  ) : (
                  <div className="li-main">
                    <div className={"li-title" + (item.status !== "ok" ? " dim" : "")}>
                      {item.status === "ok" ? item.text || <i>Empty</i>
                        : item.status === "cancelled" ? item.text || "Cancelled" : item.error || "Failed"}
                    </div>
                    {original === item.id && <div className="li-original" aria-label="Original transcript">{item.rawText}</div>}
                    <div className="li-meta">
                      {[item.appName,
                        item.mode === "command" ? "Command" : null,
                        item.status === "error" ? "Error" : null,
                        item.status === "cancelled" && item.text ? "Discarded" : null,
                        item.status === "ok" && item.latencyMs > 0 ? `${item.latencyMs} ms` : null,
                      ].filter(Boolean).join(" · ")}
                      {item.status === "ok" && item.mode === "dictate" && item.rawText && item.rawText !== item.text && (
                        <button className="link meta-link" aria-expanded={original === item.id}
                          onClick={() => setOriginal(original === item.id ? null : item.id)}>
                          {original === item.id ? "Hide original" : "Show original"}
                        </button>
                      )}
                      {item.status === "error" && item.hasAudio && (retrying === item.id
                        ? <span className="meta-link" role="status">Retrying…</span>
                        : <button className="link meta-link" onClick={() => retranscribe(item)} disabled={retrying !== null}>Retry</button>)}
                    </div>
                    {retryError?.id === item.id && <div className="li-meta"><span className="error">{retryError.message}</span></div>}
                  </div>
                  )}
                  <div className="li-actions">
                    {item.status === "ok" && item.text && editing?.id !== item.id && (
                      <button className="icon-btn" onClick={() => setEditing({ id: item.id, text: item.text })} title="Correct" aria-label="Correct">
                        <Icon name="edit" />
                      </button>
                    )}
                    {item.status === "ok" && item.mode === "dictate" && item.rawText && settings.llm.enabled && editing?.id !== item.id && (
                      <button className="icon-btn" onClick={() => repolish(item)} disabled={polishing !== null}
                        title="Polish again with AI" aria-label="Polish again with AI">
                        {polishing === item.id ? <span className="spinner" aria-hidden="true" /> : <Icon name="sparkle" />}
                      </button>
                    )}
                    {item.status === "ok" && item.hasAudio && editing?.id !== item.id && (
                      <button className="icon-btn" onClick={() => retranscribe(item)} disabled={retrying !== null}
                        title="Transcribe again" aria-label="Transcribe again">
                        {retrying === item.id ? <span className="spinner" aria-hidden="true" /> : <Icon name="mic" />}
                      </button>
                    )}
                    {(item.status === "ok" || item.status === "cancelled") && item.text && (
                      <button className="icon-btn" onClick={() => copy(item)} title="Copy" aria-label="Copy">
                        <Icon name={copied === item.id ? "check" : "copy"} />
                      </button>
                    )}
                    <button className="icon-btn" onClick={() => remove(item.id)} title="Delete" aria-label="Delete"><Icon name="trash" /></button>
                  </div>
                </div>
              ))}
            </div>
          </div>
        ))}
        {items.length > shown && (
          <button className="btn more" onClick={() => setShown(shown + PAGE_SIZE)}>Show more</button>
        )}
      </Section>
    </Page>
  );
}

const PAGE_SIZE = 100;

export function Stat({ label, value }: { label: string; value: string | number }) {
  return (
    <div className="stat">
      <div className="stat-label">{label}</div>
      <div className="stat-value">{value}</div>
    </div>
  );
}

const fmtFix = (c: { phrase: string; replacement: string | null }) => (c.replacement ? `${c.phrase} → ${c.replacement}` : c.phrase);
