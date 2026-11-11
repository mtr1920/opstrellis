import { useEffect, useRef, useState } from "react";
import { api, errMsg, useEvent, type Note, type Settings } from "../api";
import { Icon } from "../icons";
import { Empty, Modal, Page, Row, Section } from "../ui";

/** Records a meeting, transcribes it while it runs, then saves notes (with an AI summary when enabled). */
export default function Notetaker({ settings }: { settings: Settings }) {
  const [active, setActive] = useState(false);
  const [startedAt, setStartedAt] = useState<string | null>(null);
  const [transcript, setTranscript] = useState("");
  const [devices, setDevices] = useState<string[]>([]);
  const [second, setSecond] = useState("");
  const [templates, setTemplates] = useState<{ id: string; name: string }[]>([]);
  const [template, setTemplate] = useState("general");
  const [followUp, setFollowUp] = useState<{ title: string; text?: string; error?: string } | null>(null);
  const draftId = useRef(0);
  const closeDraft = () => { draftId.current++; setFollowUp(null); };
  useEffect(() => () => { draftId.current++; }, []);
  const [copied, setCopied] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [meetings, setMeetings] = useState<Note[]>([]);
  const [open, setOpen] = useState<number | null>(null);
  const [now, setNow] = useState(Date.now());

  const loadMeetings = () => api.listNotes("meeting").then(setMeetings, () => {});

  const loadStatus = () =>
    api.meetingStatus().then((s) => { setActive(s.active); setBusy(s.finishing && !s.active); setStartedAt(s.startedAt);
      // After Stop the backend's slot is already empty; keep showing the transcript while it finishes.
      if (s.active || s.transcript) setTranscript(s.transcript);
    }, () => {});
  useEvent("meeting-status", loadStatus);
  useEffect(() => {
    loadStatus();
    api.listInputDevices().then(setDevices, () => {});
    api.meetingTemplates().then(setTemplates, () => {});
    loadMeetings();
  }, []);
  useEffect(() => {
    if (!active) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [active]);

  useEvent("meeting-transcript", setTranscript);
  useEvent("meeting-done", (d) => {
    setBusy(false);
    if (d.error) setError(d.error);
    loadMeetings().then(() => d.noteId && setOpen(d.noteId));
  });

  async function start() {
    setError(null);
    try {
      await api.meetingStart(second || null, template);
      setActive(true);
      setStartedAt(new Date().toISOString());
      setTranscript("");
    } catch (e) { setError(errMsg(e)); }
  }

  async function stop() {
    try {
      await api.meetingStop();
      setActive(false);
      setBusy(true);
    } catch (e) { setError(errMsg(e)); }
  }

  async function draft(note: Note, kind: "email" | "crm") {
    const id = ++draftId.current;
    const title = kind === "email" ? "Follow-up email" : "CRM note";
    setFollowUp({ title });
    try {
      const text = await api.meetingFollowUp(note.id, kind);
      if (draftId.current === id) setFollowUp({ title, text });
    } catch (e) { if (draftId.current === id) setFollowUp({ title, error: errMsg(e) }); }
  }
  async function copy(text: string, what: string) {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(what);
      setTimeout(() => setCopied((c) => (c === what ? null : c)), 1500);
    } catch (e) { setError(errMsg(e)); }
  }

  const elapsed = startedAt ? Math.max(0, Math.floor((now - new Date(startedAt).getTime()) / 1000)) : 0;
  const clock = `${Math.floor(elapsed / 60)}:${String(elapsed % 60).padStart(2, "0")}`;

  return (
    <Page title="Notetaker" actions={!active && !busy &&
      <button className="btn primary" onClick={start}><Icon name="record" />Start recording</button>}>
      {(active || busy) && (
        <div className={"card meeting-live" + (active ? " live" : "")}>
          <div className="meeting-bar">
            <span className={"rec-dot" + (active ? "" : " idle")} aria-hidden="true" />
            <span className="meeting-state">{active ? "Recording" : "Finishing transcript and summary…"}</span>
            {active && <span className="clock">{clock}</span>}
            {active && <button className="btn" onClick={stop}><Icon name="stop" />Stop</button>}
          </div>
          <p className="meeting-text">{transcript || <span className="dim">The transcript appears here every few seconds…</span>}</p>
        </div>
      )}
      {error && <div className="error">{error}</div>}

      <div className="card">
        <Row title="Notes format" desc={settings.llm.enabled ? "The AI summary follows this template." : "Templates need AI polish (Settings → AI polish)."}>
          <select value={template} onChange={(e) => setTemplate(e.target.value)} disabled={active || !settings.llm.enabled} aria-label="Notes format">
            {templates.map((t) => <option key={t.id} value={t.id}>{t.name}</option>)}
          </select>
        </Row>
        <Row title="Also record" desc="Record a second input to capture other participants, e.g. a “Monitor of …” device or BlackHole. The transcript labels it “Them” and your microphone “Me”.">
          <select value={second} onChange={(e) => setSecond(e.target.value)} disabled={active}>
            <option value="">Microphone only</option>
            {devices.filter((d) => d !== settings.microphone).map((d) => <option key={d} value={d}>{d}</option>)}
          </select>
        </Row>
      </div>

      <Section title="Past meetings">
        {meetings.length === 0 ? (
          <Empty>
            No meetings yet. Recordings are transcribed as they run and saved here
            {settings.llm.enabled ? " with a summary and action items." : ". Turn on AI polish in Settings → AI polish for summaries."}
          </Empty>
        ) : (
          <div className="card list">
            {meetings.map((m) => (
              <div key={m.id} className={"list-row meeting-item" + (open === m.id ? " open" : "")}>
                <button className="meeting-toggle" onClick={() => setOpen(open === m.id ? null : m.id)} aria-expanded={open === m.id}>
                  <span className="li-main">
                    <span className="li-title">{m.title}</span>
                    {open !== m.id && <span className="li-meta ellipsis">{m.body.replace(/[#*\n]+/g, " ").slice(0, 160)}</span>}
                  </span>
                  <span className="chev"><Icon name="down" /></span>
                </button>
                {open === m.id && (
                  <>
                    <NoteBody text={m.body} />
                    <div className="meeting-actions">
                      <button className="btn" onClick={() => draft(m, "email")} disabled={!settings.llm.enabled}>
                        <Icon name="sparkle" />Draft follow-up email
                      </button>
                      <button className="btn" onClick={() => draft(m, "crm")} disabled={!settings.llm.enabled}>
                        <Icon name="sparkle" />CRM note
                      </button>
                      {actionItems(m.body) && (
                        <button className="btn" onClick={() => copy(actionItems(m.body), `actions-${m.id}`)}>
                          <Icon name={copied === `actions-${m.id}` ? "check" : "copy"} />Copy action items
                        </button>
                      )}
                    </div>
                  </>
                )}
              </div>
            ))}
          </div>
        )}
      </Section>

      {followUp && (
        <Modal title={followUp.title} onClose={closeDraft}
          footer={<>
            <button className="btn" onClick={closeDraft}>Close</button>
            <button className="btn primary" disabled={!followUp.text} onClick={() => followUp.text && copy(followUp.text, "follow-up")}>
              <Icon name={copied === "follow-up" ? "check" : "copy"} />{copied === "follow-up" ? "Copied" : "Copy"}
            </button>
          </>}>
          {followUp.error ? <div className="error">{followUp.error}</div>
            : followUp.text ? <pre className="follow-up">{followUp.text}</pre>
            : <p className="meta"><span className="spinner" aria-hidden="true" /> Drafting from the meeting notes…</p>}
        </Modal>
      )}
    </Page>
  );
}

/** The note's "- [ ] owner: task" lines, ready to paste into a task tracker. */
function actionItems(body: string) {
  return body.split("\n").filter((l) => /^- \[[ x]\] /.test(l)).join("\n");
}

/** Renders the few Markdown constructs meeting notes use: headings, bullets and checkboxes. */
function NoteBody({ text }: { text: string }) {
  return (
    <div className="meeting-note">
      {text.split("\n").map((line, i) => {
        if (line.startsWith("## ")) return <h4 key={i}>{line.slice(3)}</h4>;
        const item = line.match(/^- (\[[ x]\] )?(.*)$/);
        if (item) return (
          <div key={i} className={"note-li" + (item[1] ? " task" : "") + (item[1]?.includes("x") ? " done" : "")}>
            <span className="note-mark" aria-hidden="true">{item[1]?.includes("x") && <Icon name="check" size={12} />}</span>{item[2]}
          </div>
        );
        return line.trim() ? <p key={i}>{line}</p> : null;
      })}
    </div>
  );
}
