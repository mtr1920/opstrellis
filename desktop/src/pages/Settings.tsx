import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import {
  api, errMsg, useEvent, type CloudStatus, type LocalModel, type Permissions, type PolishModel, type WaylandShortcuts, type ProviderKind, type Settings, type SttEntry,
} from "../api";
import { Icon } from "../icons";
import { IS_MAC, Keys, isSingleKey, Modal, Page, Progress, Row, Search, Section, Toggle, comboText, useModelDownloads, type SettingsProps } from "../ui";

export const PROVIDERS: Record<ProviderKind, string> = {
  local: "Local model (offline)", openai: "OpenAI-compatible", deepgram: "Deepgram", assemblyai: "AssemblyAI",
  google: "Google Cloud", azure: "Azure Speech", elevenlabs: "ElevenLabs", cloud: "OpsTrellis Cloud",
};
export const MODELS: Record<Exclude<ProviderKind, "local">, string[]> = {
  openai: ["gpt-4o-transcribe", "gpt-4o-mini-transcribe", "whisper-1"],
  deepgram: ["nova-3", "nova-2"],
  assemblyai: ["universal-streaming", "best", "nano"],
  google: ["latest_long", "latest_short"],
  azure: ["default"],
  elevenlabs: ["scribe_v1"],
  cloud: ["auto", "deepgram/nova-3", "groq/whisper-large-v3-turbo", "local/moonshine-base"],
};
const STREAMING: ProviderKind[] = ["local", "openai", "deepgram", "assemblyai", "cloud"];
const LANGUAGES: [string, string][] = [
  ["auto", "Detect automatically"], ["en", "English"], ["es", "Spanish"], ["fr", "French"], ["de", "German"],
  ["it", "Italian"], ["pt", "Portuguese"], ["nl", "Dutch"], ["pl", "Polish"], ["ru", "Russian"], ["uk", "Ukrainian"],
  ["tr", "Turkish"], ["sv", "Swedish"], ["da", "Danish"], ["no", "Norwegian"], ["fi", "Finnish"], ["ar", "Arabic"],
  ["hi", "Hindi"], ["ja", "Japanese"], ["ko", "Korean"], ["zh", "Chinese"],
];
const GROUPS: [string, string[]][] = [
  ["App", ["General"]],
  ["Dictation", ["Shortcuts", "Voice input", "Text insertion", "Permissions"]],
  ["Speech & AI", ["Transcription", "Local models", "AI polish"]],
  ["Account & privacy", ["Privacy", "Account", "About"]],
];
const SECTIONS = GROUPS.flatMap(([, sections]) => sections);

type Props = SettingsProps & { saveError: string | null; saveNow: () => Promise<void> };

export default function SettingsPage(props: Props) {
  const { settings, set } = props;
  const [models, setModels] = useState<LocalModel[]>([]);
  const loadModels = () => api.listLocalModels().then(setModels, () => {});
  useEffect(() => void loadModels(), []);
  const current = useScrollSpy();

  return (
    <Page title="Settings" className="settings-page">
      <div className="settings">
        <nav className="subnav" aria-label="Settings sections">
          {GROUPS.map(([group, sections]) => (
            <div key={group} className="subnav-group">
              <div className="subnav-label">{group}</div>
              {sections.map((s) => (
                <button key={s} className={current === anchor(s) ? "active" : ""} aria-current={current === anchor(s) ? "true" : undefined}
                  onClick={() => document.getElementById(anchor(s))?.scrollIntoView({ behavior: "smooth" })}>{s}</button>
              ))}
            </div>
          ))}
        </nav>
        <div className="settings-body">
          <SettingsSection title="General">
            <Row title="Appearance">
              <div className="segmented" role="radiogroup" aria-label="Appearance">
                {(["system", "light", "dark"] as const).map((t) => (
                  <button key={t} role="radio" aria-checked={settings.theme === t} className={settings.theme === t ? "active" : ""}
                    onClick={() => set("theme", t)}>{t[0].toUpperCase() + t.slice(1)}</button>
                ))}
              </div>
            </Row>
            <Row title="Show recording pill" desc="A small indicator at the bottom of the screen while you talk.">
              <Toggle checked={settings.overlay} onChange={(v) => set("overlay", v)} />
            </Row>
            <Row title="Sounds" desc="Play a soft sound when recording starts and stops.">
              <Toggle checked={settings.sounds} onChange={(v) => set("sounds", v)} />
            </Row>
            <Row title="Launch at login">
              <Toggle checked={settings.launchAtLogin} onChange={(v) => set("launchAtLogin", v)} />
            </Row>
            <Row title={IS_MAC ? "Show in Dock" : "Show in dock and taskbar"}
              desc={IS_MAC
                ? "Off: OpsTrellis stays in the menu bar only and keeps running in the background. Open it from the menu bar icon."
                : "Off: OpsTrellis stays in the system tray only and keeps running in the background. Some Wayland desktops still show it in the dock."}>
              <Toggle checked={settings.showInDock} onChange={(v) => set("showInDock", v)} label={IS_MAC ? "Show in Dock" : "Show in dock and taskbar"} />
            </Row>
          </SettingsSection>

          <SettingsSection title="Shortcuts">
            <Shortcuts {...props} />
            {props.saveError && <div className="error">{props.saveError}</div>}
          </SettingsSection>

          <SettingsSection title="Voice input">
            <Microphone {...props} />
            <Row title="Spoken language" desc="Setting it explicitly is faster and more accurate than detection.">
              <select value={settings.language} onChange={(e) => set("language", e.target.value)}>
                {LANGUAGES.map(([code, name]) => <option key={code} value={code}>{name}</option>)}
              </select>
            </Row>
          </SettingsSection>

          <SettingsSection title="Text insertion">
            <Row title="Insert text by" desc="Pasting is instant. Typing works in apps that block paste.">
              <select value={settings.insertion.method}
                onChange={(e) => set("insertion", { ...settings.insertion, method: e.target.value as "paste" | "type" })}>
                <option value="paste">Pasting</option>
                <option value="type">Typing</option>
              </select>
            </Row>
            <Row title="Restore clipboard" desc="Put back what was on your clipboard after pasting.">
              <Toggle checked={settings.insertion.restoreClipboard} onChange={(v) => set("insertion", { ...settings.insertion, restoreClipboard: v })} />
            </Row>
            <Row title="Paste delay" desc="Increase if text sometimes doesn't appear.">
              <div className="inline">
                <input type="number" min={0} max={2000} step={10} className="num" value={settings.insertion.pasteDelayMs}
                  onChange={(e) => set("insertion", { ...settings.insertion, pasteDelayMs: Math.min(2000, Math.max(0, Math.round(Number(e.target.value) || 0))) })} />
                <span className="meta">ms</span>
              </div>
            </Row>
          </SettingsSection>

          <SettingsSection title="Permissions"><PermissionsInfo shortcuts={settings.hotkeys.dictate + " " + settings.hotkeys.command} withShortcutSetup={false} /></SettingsSection>

          <SettingsSection title="Transcription" desc="OpsTrellis uses the first enabled provider. If it fails, the next one takes over.">
            <Providers {...props} localModels={models} />
          </SettingsSection>

          <SettingsSection title="Local models" desc="Run speech recognition on this computer. Nothing leaves your device.">
            <LocalModels models={models} reload={loadModels} settings={settings} set={set} />
          </SettingsSection>

          <SettingsSection title="AI polish" desc="AI polish cleans up dictations, runs transforms and command mode, and summarizes meetings.">
            <LlmSection settings={settings} set={set} saveNow={props.saveNow} />
          </SettingsSection>

          <SettingsSection title="Privacy"><Privacy {...props} /></SettingsSection>
          <SettingsSection title="Account"><Account {...props} /></SettingsSection>

          <SettingsSection title="About"><About /></SettingsSection>

        </div>
      </div>
    </Page>
  );
}

const anchor = (title: string) => "s-" + title.toLowerCase().replace(/\s+/g, "-");

/** The id of the section nearest the top of the scrolling panel, for highlighting the section nav. */
function useScrollSpy() {
  const [current, setCurrent] = useState(anchor(SECTIONS[0]));
  const frame = useRef(0);
  useEffect(() => {
    const panel = document.querySelector(".panel");
    if (!panel) return;
    const update = () => {
      cancelAnimationFrame(frame.current);
      frame.current = requestAnimationFrame(() => {
        const top = panel.getBoundingClientRect().top + 120;
        const atEnd = panel.scrollTop + panel.clientHeight >= panel.scrollHeight - 4;
        let id = anchor(SECTIONS[0]);
        for (const s of SECTIONS) {
          const el = document.getElementById(anchor(s));
          if (el && (el.getBoundingClientRect().top <= top || atEnd)) id = anchor(s);
        }
        setCurrent(id);
      });
    };
    panel.addEventListener("scroll", update, { passive: true });
    return () => { panel.removeEventListener("scroll", update); cancelAnimationFrame(frame.current); };
  }, []);
  return current;
}

function SettingsSection({ title, desc, children }: { title: string; desc?: string; children: ReactNode }) {
  return <Section id={anchor(title)} title={title} desc={desc}><div className="card">{children}</div></Section>;
}

// ---------- AI ----------

const LLM_DEFAULTS: Record<Settings["llm"]["provider"], string> = {
  openai: "gpt-4o-mini", anthropic: "claude-haiku-4-5", cloud: "auto", local: "s1-mini",
};

/** AI polish on/off, provider, model, key, and a test request. */
export function LlmSection({ settings, set, saveNow }: SettingsProps & { saveNow: () => Promise<void> }) {
  const llm = settings.llm;
  const update = (patch: Partial<Settings["llm"]>) => set("llm", { ...llm, ...patch });
  const [key, setKey] = useState("");
  const [hasKey, setHasKey] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [test, setTest] = useState<{ busy?: boolean; ok?: string; error?: string }>({});
  useEffect(() => void api.hasSecrets().then((s) => setHasKey(s.includes("llm")), () => {}), []);

  async function saveKey() {
    try {
      await api.setSecret("llm", key.trim());
      setKey(""); setHasKey(true); setMsg(null);
    } catch (e) { setMsg(errMsg(e)); }
  }
  async function runTest() {
    setTest({ busy: true });
    try {
      await saveNow(); // test what's on screen, not the last debounced save
      const r = await api.testLlm();
      setTest({ ok: `“${r.text}” in ${r.latencyMs} ms` });
    } catch (e) { setTest({ error: errMsg(e) }); }
  }

  return (
    <>
      <Row title="AI polish" desc={"Fix grammar, handle self-corrections, format lists and apply your style. " +
        (llm.provider === "local" ? "Runs on this device: nothing leaves your computer." : "Dictated text is sent to the provider below.")}>
        <Toggle checked={llm.enabled} onChange={(v) => update({ enabled: v })} label="AI polish" />
      </Row>
      {llm.enabled && (
        <div className="form-grid">
          <label className="field">
            <span>Provider</span>
            <select value={llm.provider} onChange={(e) => {
              const provider = e.target.value as Settings["llm"]["provider"];
              update({ provider, model: LLM_DEFAULTS[provider], baseUrl: null });
              setTest({});
              // The saved key belongs to the previous provider; never send it to the new one.
              if (hasKey) api.deleteSecret("llm").then(() => setHasKey(false), (err) => setMsg(errMsg(err)));
            }}>
              <option value="openai">OpenAI-compatible</option>
              <option value="anthropic">Anthropic</option>
              <option value="cloud">OpsTrellis Cloud</option>
              <option value="local">On this device (S1-mini, offline)</option>
            </select>
          </label>
          {llm.provider !== "local" && (
            <label className="field">
              <span>Model</span>
              <input value={llm.model} onChange={(e) => update({ model: e.target.value })} />
            </label>
          )}
          {llm.provider === "local" && <LocalPolish />}
          {llm.provider === "openai" && (
            <label className="field span2">
              <span>Base URL <em>optional: Groq, OpenRouter, Ollama, LM Studio…</em></span>
              <input value={llm.baseUrl ?? ""} placeholder="https://api.openai.com/v1"
                onChange={(e) => update({ baseUrl: e.target.value || null })} />
            </label>
          )}
          {llm.provider !== "cloud" && llm.provider !== "local" && (
            <label className="field span2">
              <span>API key {hasKey && <em className="ok">saved</em>}</span>
              <div className="inline">
                <input type="password" value={key} placeholder={hasKey ? "••••••••  (enter to replace)" : "Paste your API key"}
                  onChange={(e) => setKey(e.target.value)} />
                <button className="btn" onClick={saveKey} disabled={!key.trim()}>Save key</button>
              </div>
              {msg && <div className="error">{msg}</div>}
            </label>
          )}
          {llm.provider === "cloud" && <p className="meta span2">Uses your OpsTrellis Cloud account. Sign in under Account.</p>}
          <div className="span2 inline test-row">
            <button className="btn" onClick={runTest} disabled={test.busy}>{test.busy ? "Testing…" : "Test"}</button>
            {test.ok && <span className="ok"><Icon name="check" />{test.ok}</span>}
            {test.error && <span className="error">{test.error}</span>}
          </div>
        </div>
      )}
    </>
  );
}

/** The on-device polish model: download, progress, delete, and what it does and doesn't do. */
function LocalPolish() {
  const [model, setModel] = useState<PolishModel | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = () => api.polishModelStatus().then(setModel, (e) => setError(errMsg(e)));
  useEffect(() => void load(), []);
  const progress = useModelDownloads((id) => id === "s1-mini" && load())["s1-mini"];
  const busy = (!!progress && !progress.error) || !!model?.downloading;
  // A retry hides the last attempt's error until the new attempt reports progress.
  const [retried, setRetried] = useState<typeof progress | undefined>(undefined);
  const failed = (progress !== retried ? progress?.error : null) ?? error;

  return (
    <div className="span2">
      <div className="row">
        <div className="row-text">
          <div className="row-title">S1-mini by Superwhisper <span className="meta">· English · {model?.sizeMb ?? 484} MB</span></div>
          <div className="row-desc">
            A small model trained only to clean up dictation: fillers, self-corrections (“at 2, sorry, 3” → “at 3”), grammar and lists.
            Uses about 1 GB of memory while selected. Code editors, terminals and other languages keep the rule-based text;
            actions, command mode and meeting summaries need one of the other providers.
          </div>
          {busy && progress && <Progress value={progress.total ? progress.downloaded / progress.total : 0} />}
        </div>
        <div className="row-control">
          {model?.installed ? (
            <>
              <span className="tag ok">Installed</span>
              <button className="icon-btn" title="Delete model"
                onClick={() => api.deletePolishModel().then(load, (e) => setError(errMsg(e)))}><Icon name="trash" /></button>
            </>
          ) : (
            <button className="btn primary" disabled={busy || !model}
              onClick={() => { setError(null); setRetried(progress); api.downloadPolishModel().catch((e) => setError(errMsg(e))); }}>
              {busy && progress ? `${Math.round((progress.downloaded / (progress.total || 1)) * 100)}%` : busy ? "Downloading…" : "Download"}
            </button>
          )}
        </div>
      </div>
      {model && !model.installed && !busy && <p className="meta">Until it's downloaded, dictation uses the rule-based cleanup.</p>}
      {failed && <div className="error">Download failed: {failed}. Check your connection and try again.</div>}
    </div>
  );
}

// ---------- Shortcuts ----------

const MODIFIER_CODES = /^(Control|Shift|Alt|Meta|OS)(Left|Right)?$/;

/** Turns a keydown into an accelerator like "Ctrl+Shift+Space"; null while only modifiers are held. */
function accelerator(e: KeyboardEvent): { mods: string[]; key: string | null } {
  const mods = [e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Super"].filter(Boolean) as string[];
  if (MODIFIER_CODES.test(e.code)) return { mods, key: null };
  const key = e.code.replace(/^Key/, "").replace(/^Digit/, "").replace(/^Arrow/, "");
  return { mods, key };
}

type Hotkeys = Settings["hotkeys"];
const SHORTCUT_NAMES: Record<keyof Hotkeys, string> = { dictate: "Dictate", command: "Command mode", pasteLast: "Paste last transcript" };

/** Dictate and command mode can also be a single key on macOS (🌐 fn, Right ⌥, …), one of them at a time. */
/** Always registered for dictation by the backend (`hotkeys::FALLBACK`) unless a shortcut above uses it. */
const FALLBACK = "Ctrl+Shift+Space";

function Shortcuts({ settings, set }: SettingsProps) {
  const hk = settings.hotkeys;
  const [taken, setTaken] = useState<string | null>(null);
  const update = (which: keyof Hotkeys, value: string) => {
    // Catch a clash here, where it can be named, instead of a rejected save.
    const other = (Object.keys(hk) as (keyof Hotkeys)[]).find((k) => k !== which && hk[k] === value);
    if (other) { setTaken(`${comboText(value)} is already the ${SHORTCUT_NAMES[other]} shortcut.`); return; }
    setTaken(null);
    set("hotkeys", { ...hk, [which]: value });
  };
  // Why a single key can't be used in a row (true: it can).
  const single = (other: keyof Hotkeys): true | string =>
    isSingleKey(hk[other]) ? `${SHORTCUT_NAMES[other]} already uses a single key; use a combination here` : true;
  const holdDesc = (base: string, value: string) =>
    isSingleKey(value) ? `${base} Double-tap for hands-free, press again to finish.` : `${base} Tap once for hands-free, tap again to finish.`;
  const [perm, reload] = usePermissions();
  const ws = perm?.waylandShortcuts;
  // The desktop owns the keys then; the fields stay only to replace a single key it can't use.
  if (ws?.portal.state === "bound" && !Object.values(hk).some(isSingleKey)) return <WaylandShortcutsSetup ws={ws} reload={reload} />;
  return (
    <>
      {ws && <WaylandShortcutsSetup ws={ws} reload={reload} />}
      <Row title="Dictate" desc={holdDesc("Hold to talk.", hk.dictate)}>
        <HotkeyInput value={hk.dictate} onChange={(v) => update("dictate", v)} single={IS_MAC && single("command")} />
      </Row>
      <Row title="Command mode" desc={holdDesc("Select text, hold and say how to change it.", hk.command)}>
        <HotkeyInput value={hk.command} onChange={(v) => update("command", v)} single={IS_MAC && single("dictate")} />
      </Row>
      <Row title="Paste last transcript">
        <HotkeyInput value={hk.pasteLast} onChange={(v) => update("pasteLast", v)}
          single={IS_MAC && "Paste last needs a key combination"} />
      </Row>
      {!Object.values(hk).includes(FALLBACK) && (
        <Row title="Fallback dictation" desc="Always on, in case the dictate shortcut stops working. Press once to start, press again to finish.">
          <Keys combo={FALLBACK} />
        </Row>
      )}
      {taken && <div className="error">{taken}</div>}
    </>
  );
}

/**
 * Records a shortcut. Combinations come from the webview's key events. Single keys (🌐 fn, Right ⌥,
 * …) never reach the webview, so on macOS the backend captures them (`shortcut-captured`); it also
 * pauses the current shortcuts meanwhile, so pressing one records it instead of triggering it.
 * `single`: true if a single key may be the shortcut, or why not (false: not offered, Linux).
 */
function HotkeyInput({ value, onChange, single }: { value: string; onChange: (v: string) => void; single: boolean | string }) {
  const [recording, setRecording] = useState(false);
  const [preview, setPreview] = useState("");

  useEffect(() => {
    if (!recording) return;
    api.setShortcutRecording(true).catch(() => {});
    return () => void api.setShortcutRecording(false).catch(() => {});
  }, [recording]);

  useEvent("shortcut-captured", (key) => {
    if (!recording) return;
    if (single !== true) { if (single) setPreview(single); return; }
    onChange(key);
    setRecording(false);
  });

  function onKeyDown(e: KeyboardEvent) {
    if (!recording) return;
    e.preventDefault();
    if (e.key === "Escape" && !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey) { setRecording(false); return; }
    // WebKit reports 🌐 as a key; it's captured natively on release (`shortcut-captured`).
    if (e.code === "Fn" || e.key === "Fn") return;
    const { mods, key } = accelerator(e);
    if (!key) { setPreview(mods.length ? comboText(mods.join("+")) + (IS_MAC ? "…" : "+…") : ""); return; }
    if (mods.length === 0 && !/^F\d+$/.test(key)) { setPreview(single === true ? "Add a modifier, or press 🌐 fn" : "Add a modifier key"); return; }
    onChange([...mods, key].join("+"));
    setRecording(false);
  }

  return (
    <div className="inline">
      {single === true && value !== "Fn" && !recording && (
        <button type="button" className="btn ghost use-fn" onClick={() => onChange("Fn")} title="Use the Fn (🌐) key">Use 🌐 fn</button>
      )}
      <button type="button" className={"hotkey" + (recording ? " recording" : "")}
        title={recording
          ? (single === true ? "Press a key combination, or a single key like 🌐 fn or Right ⌥. Esc keeps the current one."
            : "Press a key combination. Esc keeps the current one.")
          : "Click to change"}
        onClick={() => { setRecording(!recording); setPreview(""); }} onKeyDown={onKeyDown} onBlur={() => setRecording(false)}>
        {recording ? preview || "Press keys…" : <Keys combo={value} />}
      </button>
    </div>
  );
}

// ---------- Microphone ----------

function Microphone({ settings, set }: SettingsProps) {
  const [devices, setDevices] = useState<string[]>([]);
  useEffect(() => void api.listInputDevices().then(setDevices, () => {}), []);
  const missing = settings.microphone && !devices.includes(settings.microphone);
  return (
    <Row title="Microphone" desc="Used for dictation and meeting notes. System default follows your sound settings.">
      <select value={settings.microphone ?? ""} onChange={(e) => set("microphone", e.target.value || null)}>
        <option value="">System default</option>
        {missing && <option value={settings.microphone!}>{settings.microphone} (not connected)</option>}
        {devices.map((d) => <option key={d} value={d}>{d}</option>)}
      </select>
    </Row>
  );
}

// ---------- Transcription providers ----------

function Providers({ settings, set, saveNow, localModels }: Props & { localModels: LocalModel[] }) {
  const chain = settings.sttChain;
  const [secrets, setSecrets] = useState<string[]>([]);
  const loadSecrets = () => api.hasSecrets().then(setSecrets, () => {});
  useEffect(() => void loadSecrets(), []);

  const update = (id: string, patch: Partial<SttEntry>) => set("sttChain", chain.map((e) => (e.id === id ? { ...e, ...patch } : e)));
  const move = (i: number, d: number) => {
    const next = [...chain];
    [next[i], next[i + d]] = [next[i + d], next[i]];
    set("sttChain", next);
  };
  const remove = (id: string) => {
    set("sttChain", chain.filter((e) => e.id !== id));
    if (secrets.includes(`stt:${id}`)) api.deleteSecret(`stt:${id}`).then(loadSecrets, () => {});
  };
  const add = () => set("sttChain", [...chain, {
    id: crypto.randomUUID(), provider: "deepgram", model: "nova-3", baseUrl: null, region: null, streaming: true, enabled: true,
  }]);

  return (
    <>
      {chain.length === 0 && <div className="list-empty">No providers yet. Add one, or download a local model below.</div>}
      {chain.map((entry, i) => (
        <ProviderEntry key={entry.id} entry={entry} index={i} count={chain.length} localModels={localModels}
          hasKey={secrets.includes(`stt:${entry.id}`)} onKeySaved={loadSecrets} saveNow={saveNow}
          update={(p) => update(entry.id, p)} move={(d) => move(i, d)} remove={() => remove(entry.id)}
          primary={chain.findIndex((e) => e.enabled) === i} />
      ))}
      <div className="row"><button className="btn" onClick={add}><Icon name="plus" />Add provider</button></div>
    </>
  );
}

interface EntryProps {
  entry: SttEntry; index: number; count: number; primary: boolean; localModels: LocalModel[]; hasKey: boolean;
  update: (p: Partial<SttEntry>) => void; move: (d: number) => void; remove: () => void;
  onKeySaved: () => void; saveNow: () => Promise<void>;
}

function ProviderEntry({ entry, index, count, primary, localModels, hasKey, update, move, remove, onKeySaved, saveNow }: EntryProps) {
  const [key, setKey] = useState("");
  const [test, setTest] = useState<{ busy?: boolean; text?: string; latencyMs?: number; error?: string } | null>(null);
  const installed = localModels.filter((m) => m.installed);
  const needsKey = entry.provider !== "local";

  async function saveKey() {
    try { await api.setSecret(`stt:${entry.id}`, key.trim()); setKey(""); onKeySaved(); }
    catch (e) { setTest({ error: errMsg(e) }); }
  }
  async function runTest() {
    setTest({ busy: true });
    try {
      await saveNow();
      const r = await api.testStt(entry.id);
      setTest(r);
    } catch (e) { setTest({ error: errMsg(e) }); }
  }

  return (
    <div className={"provider" + (entry.enabled ? "" : " disabled")}>
      <div className="provider-head">
        <span className="order">{index + 1}</span>
        <span className="strong">{PROVIDERS[entry.provider]}</span>
        <span className="tag">{!entry.enabled ? "Off" : primary ? "Primary" : "Fallback"}</span>
        <span className="grow" />
        <button className="icon-btn" title="Move up" disabled={index === 0} onClick={() => move(-1)}><Icon name="up" /></button>
        <button className="icon-btn" title="Move down" disabled={index === count - 1} onClick={() => move(1)}><Icon name="down" /></button>
        <button className="icon-btn" title="Remove" onClick={remove}><Icon name="trash" /></button>
        <Toggle checked={entry.enabled} onChange={(v) => update({ enabled: v })} label="Enabled" />
      </div>
      <div className="form-grid">
        <label className="field">
          <span>Provider</span>
          <select value={entry.provider} onChange={(e) => {
            const provider = e.target.value as ProviderKind;
            const model = provider === "local" ? installed[0]?.id ?? "moonshine-tiny-en" : MODELS[provider][0];
            update({ provider, model, streaming: STREAMING.includes(provider) && entry.streaming });
          }}>
            {Object.entries(PROVIDERS).map(([id, label]) => <option key={id} value={id}>{label}</option>)}
          </select>
        </label>
        <label className="field">
          <span>Model</span>
          {entry.provider === "local" ? (
            <select value={entry.model} onChange={(e) => update({ model: e.target.value })}>
              {!installed.some((m) => m.id === entry.model) && (
                <option value={entry.model}>
                  {localModels.find((m) => m.id === entry.model)?.name ?? entry.model}{localModels.length ? " (not downloaded)" : ""}
                </option>
              )}
              {installed.map((m) => <option key={m.id} value={m.id}>{m.name}</option>)}
            </select>
          ) : (
            <>
              <input list={`models-${entry.id}`} value={entry.model} onChange={(e) => update({ model: e.target.value })} />
              <datalist id={`models-${entry.id}`}>{MODELS[entry.provider].map((m) => <option key={m} value={m} />)}</datalist>
            </>
          )}
        </label>
        {(entry.provider === "openai" || entry.provider === "cloud") && (
          <label className="field span2">
            <span>Base URL <em>{entry.provider === "openai" ? "optional: Groq, Fireworks, self-hosted…" : "optional: self-hosted gateway"}</em></span>
            <input value={entry.baseUrl ?? ""} placeholder={entry.provider === "openai" ? "https://api.openai.com/v1" : "Uses your OpsTrellis Cloud URL"}
              onChange={(e) => update({ baseUrl: e.target.value || null })} />
          </label>
        )}
        {entry.provider === "azure" && (
          <label className="field">
            <span>Region</span>
            <input value={entry.region ?? ""} placeholder="westeurope" onChange={(e) => update({ region: e.target.value || null })} />
          </label>
        )}
        {needsKey && (
          <label className="field span2">
            <span>API key {hasKey && <em className="ok">saved</em>}
              {entry.provider === "cloud" && !hasKey && <em>optional when signed in</em>}</span>
            <div className="inline">
              <input type="password" value={key} placeholder={hasKey ? "••••••••  (enter to replace)" : "Paste your API key"}
                onChange={(e) => setKey(e.target.value)} onKeyDown={(e) => e.key === "Enter" && key.trim() && saveKey()} />
              <button className="btn" onClick={saveKey} disabled={!key.trim()}>Save key</button>
            </div>
          </label>
        )}
      </div>
      <div className="provider-foot">
        {STREAMING.includes(entry.provider) ? (
          <label className="toggle-label">
            <Toggle checked={entry.streaming} onChange={(v) => update({ streaming: v })} label="Streaming" />Stream while speaking
          </label>
        ) : <span className="meta">Transcribes after you finish speaking</span>}
        <span className="grow" />
        {test && !test.busy && (test.error
          ? <span className="error inline-msg">{test.error}</span>
          : <span className="ok inline-msg"><Icon name="check" />“{test.text}” · {test.latencyMs} ms</span>)}
        <button className="btn" onClick={runTest} disabled={test?.busy}>{test?.busy ? "Testing…" : "Test"}</button>
      </div>
    </div>
  );
}

// ---------- Local models ----------

type ModelSort = "recommended" | "size" | "name";
const MODEL_SORTS: [ModelSort, string][] = [["recommended", "Recommended"], ["size", "Smallest download"], ["name", "Name"]];
const languageNames = (() => {
  try { return new Intl.DisplayNames(["en"], { type: "language" }); } catch { return null; }
})();
/** "de" → "German". Whisper still uses "jw" for Javanese. */
const languageName = (code: string) => languageNames?.of(code === "jw" ? "jv" : code) ?? code;

/** Where a local model sits in the provider chain: the first enabled local entry using it, if any. */
function chainRole(chain: SttEntry[], id: string): "Primary" | "Fallback" | null {
  const i = chain.findIndex((e) => e.enabled && e.provider === "local" && e.model === id);
  if (i < 0) return null;
  return chain.findIndex((e) => e.enabled) === i ? "Primary" : "Fallback";
}

/** A searchable catalog of on-device models: filter by language, download, cancel, use and delete. */
function LocalModels({ models, reload, settings, set }: SettingsProps & { models: LocalModel[]; reload: () => void }) {
  const [pending, setPending] = useState<Set<string>>(new Set());
  const settle = (id: string) => setPending((prev) => { const next = new Set(prev); next.delete(id); return next; });
  const progress = useModelDownloads((id) => { settle(id); reload(); });
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [lang, setLang] = useState(settings.language === "auto" ? "all" : settings.language);
  const [sort, setSort] = useState<ModelSort>("recommended");
  const [downloadedOnly, setDownloadedOnly] = useState(false);
  const failed = models.filter((m) => !pending.has(m.id)).map((m) => progress[m.id]).find((p) => p?.error);
  const chain = settings.sttChain;

  const languages = [...new Set(models.flatMap((m) => m.langs))]
    .map((code) => [code, languageName(code)] as const)
    .sort((a, b) => a[1].localeCompare(b[1]));
  const busy = (m: LocalModel) => {
    const p = progress[m.id];
    return pending.has(m.id) || (p ? !p.error && !p.done : m.downloading);
  };
  const rank = (m: LocalModel) => chainRole(chain, m.id) ? 0 : m.installed ? 1 : busy(m) ? 2 : m.recommended ? 3 : 4;
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const shown = models
    .map((m, order) => ({ m, order, text: [m.name, m.description, m.languages, ...m.langs.map(languageName)].join(" ").toLowerCase() }))
    .filter(({ m, text }) => (lang === "all" || m.langs.includes(lang)) && (!downloadedOnly || m.installed) && words.every((w) => text.includes(w)))
    .sort((a, b) =>
      sort === "size" ? a.m.sizeMb - b.m.sizeMb || a.order - b.order
        : sort === "name" ? a.m.name.localeCompare(b.m.name)
          : rank(a.m) - rank(b.m) || a.order - b.order)
    .map(({ m }) => m);
  const filtered = !!words.length || lang !== "all" || downloadedOnly;

  function download(id: string) {
    setError(null);
    setPending((prev) => new Set(prev).add(id));
    api.downloadLocalModel(id).then(() => settle(id), (e) => { settle(id); setError(errMsg(e)); });
  }
  /** Points the enabled local provider at this model, keeping its place in the chain. Otherwise a
   * local entry goes last, so it never takes over from a provider the user has on. */
  function use(id: string) {
    const on = chain.findIndex((e) => e.enabled && e.provider === "local");
    if (on >= 0) return set("sttChain", chain.map((e, j) => (j === on ? { ...e, model: id } : e)));
    const off = chain.find((e) => e.provider === "local");
    const entry: SttEntry = off
      ? { ...off, model: id, enabled: true }
      : { id: crypto.randomUUID(), provider: "local", model: id, baseUrl: null, region: null, streaming: true, enabled: true };
    set("sttChain", [...chain.filter((e) => e !== off), entry]);
  }

  return (
    <>
      <div className="model-tools">
        <Search value={query} onChange={setQuery} placeholder="Search models or languages" />
        <select value={lang} onChange={(e) => setLang(e.target.value)} aria-label="Language">
          <option value="all">All languages</option>
          {languages.map(([code, name]) => <option key={code} value={code}>{name}</option>)}
        </select>
        <select value={sort} onChange={(e) => setSort(e.target.value as ModelSort)} aria-label="Sort by">
          {MODEL_SORTS.map(([id, label]) => <option key={id} value={id}>{label}</option>)}
        </select>
        <label className="toggle-label">
          <Toggle checked={downloadedOnly} onChange={setDownloadedOnly} label="Downloaded only" />Downloaded only
        </label>
        <span className="meta model-count">{shown.length === models.length ? `${models.length} models` : `${shown.length} of ${models.length}`}</span>
      </div>
      {shown.map((m) => {
        const p = progress[m.id];
        const role = chainRole(chain, m.id);
        const downloading = busy(m);
        const pct = p?.total && !p.done ? Math.min(100, Math.floor((p.downloaded / p.total) * 100)) : null;
        return (
          <div key={m.id} className="row model-row">
            <div className="row-text">
              <div className="row-title">{m.name}{m.recommended && <span className="tag" title="A good default for the languages it covers.">Recommended</span>}</div>
              <div className="row-desc">{m.description}</div>
              <div className="row-desc model-facts">{m.languages} · {m.sizeMb >= 1000 ? `${(m.sizeMb / 1000).toFixed(1)} GB` : `${m.sizeMb} MB`}</div>
              {downloading && <Progress value={pct === null ? 0 : pct / 100} />}
            </div>
            <div className="row-control">
              {downloading && pct === 100 ? (
                <span className="meta">Installing…</span>
              ) : downloading ? (
                <>
                  <span className="meta model-pct">{pct === null ? "Starting…" : `${pct}%`}</span>
                  <button className="icon-btn" title="Cancel download" aria-label={`Cancel ${m.name} download`}
                    onClick={() => api.cancelLocalDownload(m.id).catch((e) => setError(errMsg(e)))}><Icon name="x" /></button>
                </>
              ) : !m.installed ? (
                <button className="btn" onClick={() => download(m.id)}>Download</button>
              ) : (
                <>
                  {role
                    ? <span className="tag ok" title={role === "Primary" ? "Dictation uses this model." : "Used when the providers above it fail."}>{role === "Primary" ? "In use" : "Fallback"}</span>
                    : <button className="btn" onClick={() => use(m.id)}>Use model</button>}
                  <button className="icon-btn" title="Delete model" aria-label={`Delete ${m.name}`}
                    onClick={() => api.deleteLocalModel(m.id).then(reload, (e) => setError(errMsg(e)))}><Icon name="trash" /></button>
                </>
              )}
            </div>
          </div>
        );
      })}
      {models.length === 0 && <div className="list-empty">No local models available.</div>}
      {models.length > 0 && shown.length === 0 && (
        <div className="list-empty">No models match.{filtered && <> <button className="link" onClick={() => { setQuery(""); setLang("all"); setDownloadedOnly(false); }}>Clear filters</button></>}</div>
      )}
      {(error || failed) && <div className="error">{error ?? `Download failed: ${failed!.error}`}</div>}
    </>
  );
}

// ---------- Privacy ----------

function Privacy({ settings, set }: SettingsProps) {
  const [confirm, setConfirm] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const r = settings.privacy.historyRetentionDays;
  const retention = r === null ? "forever" : String(r);

  return (
    <>
      <Row title="Keep history" desc={r === 0 ? "Privacy mode: dictations are not saved." : "Older dictations are deleted automatically."}>
        <select value={retention}
          onChange={(e) => set("privacy", { ...settings.privacy, historyRetentionDays: e.target.value === "forever" ? null : Number(e.target.value) })}>
          <option value="forever">Forever</option>
          <option value="30">30 days</option>
          <option value="7">7 days</option>
          <option value="0">Don't save</option>
        </select>
      </Row>
      <Row title="Sync usage stats" desc="Send word counts and durations (never text or audio) to your OpsTrellis Cloud account.">
        <Toggle checked={settings.privacy.syncUsage} onChange={(v) => set("privacy", { ...settings.privacy, syncUsage: v })} />
      </Row>
      <Row title="Clear history" desc={msg ?? "Permanently delete all saved dictations."}>
        <button className="btn danger" onClick={() => setConfirm(true)}>Clear all history</button>
      </Row>
      {confirm && (
        <Modal title="Clear all history?" onClose={() => setConfirm(false)}
          footer={<>
            <button className="btn" onClick={() => setConfirm(false)}>Cancel</button>
            <button className="btn danger solid" onClick={async () => {
              try { await api.clearHistory(); setMsg("History cleared."); } catch (e) { setMsg(errMsg(e)); }
              setConfirm(false);
            }}>Delete everything</button>
          </>}>
          This deletes every saved dictation on this computer. Your stats history is removed too. This can't be undone.
        </Modal>
      )}
    </>
  );
}

// ---------- Account ----------

function Account({ settings, set }: SettingsProps) {
  const [status, setStatus] = useState<CloudStatus | null>(null);
  const [login, setLogin] = useState<{ userCode: string; verificationUrl: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => void api.cloudStatus().then(setStatus, (e) => setError(errMsg(e))), []);
  useEvent("cloud-status", (s) => { setStatus(s); setLogin(null); }); // signed in, or the attempt ended

  async function signIn() {
    setError(null);
    try { setLogin(await api.cloudLogin()); } catch (e) { setError(errMsg(e)); }
  }
  async function signOut() {
    try { setStatus(await api.cloudLogout()); } catch (e) { setError(errMsg(e)); }
  }

  return (
    <>
      <Row title="OpsTrellis Cloud" desc={status?.signedIn
        ? <>Signed in as <b>{status.email}</b>{status.plan && <> · <span className="tag">{status.plan}</span></>}</>
        : "Optional. Use managed transcription without your own keys and sync your stats."}>
        {status?.signedIn
          ? <button className="btn" onClick={signOut}>Sign out</button>
          : <button className="btn primary" onClick={signIn}>Sign in</button>}
      </Row>
      {status?.signedIn && status.usage && typeof status.usage === "object" && (
        <div className="row usage">
          {Object.entries(status.usage).filter(([, v]) => typeof v !== "object").map(([k, v]) => (
            <div key={k}><div className="meta">{humanize(k)}</div><div className="strong">{String(v)}</div></div>
          ))}
        </div>
      )}
      {login && !status?.signedIn && (
        <div className="row device-code">
          <div>
            <div className="meta">We opened your browser. Confirm this code there:</div>
            <div className="code">{login.userCode}</div>
            <div className="meta">Browser didn't open? Go to <span className="mono">{login.verificationUrl}</span></div>
          </div>
          <span className="spinner" aria-label="Waiting for approval" />
        </div>
      )}
      <Row title="Server URL" desc="Change this only if you run your own OpsTrellis Cloud.">
        <input className="wide" value={settings.cloud.baseUrl} onChange={(e) => set("cloud", { ...settings.cloud, baseUrl: e.target.value })} />
      </Row>
      {error && <div className="error">{error}</div>}
    </>
  );
}

const humanize = (k: string) => k.replace(/([A-Z])/g, " $1").replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());

// ---------- About ----------

function About() {
  const [version, setVersion] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => void api.version().then(setVersion, () => {}), []);

  return (
    <>
      <Row title="Version" desc="MIT licensed: free for anyone, including commercial use."><span className="meta mono">{version ?? "…"}</span></Row>
      <Row title="Logs" desc={error ?? "Timings and errors for each dictation, without the dictated text."}>
        <button className="btn" onClick={() => api.openLogs().then(() => setError(null), (e) => setError(errMsg(e)))}>Show logs</button>
      </Row>
    </>
  );
}

// ---------- Permissions ----------

/**
 * The backend's permissions report. Reloads when `key` changes (after the debounced save
 * re-registers the shortcuts), when the window regains focus (permissions are granted in the
 * system's settings) and when the backend says something changed.
 */
function usePermissions(key?: string): [Permissions | null, () => void] {
  const [perm, setPerm] = useState<Permissions | null>(null);
  const load = () => void api.permissions().then(setPerm, () => {});
  const first = useRef(true);
  useEffect(() => {
    const t = setTimeout(load, first.current ? 0 : 800);
    first.current = false;
    return () => clearTimeout(t);
  }, [key]);
  useEffect(() => {
    window.addEventListener("focus", load);
    return () => window.removeEventListener("focus", load);
  }, []);
  useEvent("permissions-changed", load);
  return [perm, load];
}

const WAYLAND_SHORTCUT_NAMES: Record<string, string> = { dictate: "Dictate", command: "Command mode", "paste-last": "Paste last transcript" };

/**
 * Wayland doesn't let apps grab keys, so shortcuts go through the desktop: its GlobalShortcuts
 * portal when it has one (hold-to-talk works), else GNOME custom shortcuts or the CLI commands.
 */
export function WaylandShortcutsSetup({ ws, reload }: { ws: WaylandShortcuts; reload: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = (f: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    f().then(reload, (e) => setError(errMsg(e))).finally(() => setBusy(false));
  };
  const p = ws.portal;

  if (p.state === "bound") return (
    <div className="row column">
      <div className="row-text">
        <div className="row-title">Shortcuts set by your desktop</div>
        <div className="row-desc">
          On Wayland your desktop handles global shortcuts. Hold to talk, or tap for hands-free.{" "}
          {ws.gnome ? "To change the keys, open GNOME Settings → Apps → OpsTrellis." : "Change the keys in your desktop's shortcut settings."}
        </div>
      </div>
      <table className="cmd-table">
        <tbody>
          {p.shortcuts.map((sc) => (
            <tr key={sc.id}><td>{WAYLAND_SHORTCUT_NAMES[sc.id] ?? sc.id}</td><td>{sc.trigger ? <Keys combo={sc.trigger} /> : "No key assigned"}</td></tr>
          ))}
        </tbody>
      </table>
    </div>
  );

  if (p.state === "binding") return (
    <Row title="Wayland shortcuts" desc="Your desktop is asking you to approve OpsTrellis's shortcuts. Click Add in its dialog.">
      <span className="tag">Waiting</span>
    </Row>
  );

  return (
    <div className="row column">
      <div className="row-text">
        <div className="row-title">Wayland shortcuts</div>
        <div className="row-desc">
          On Wayland apps can't listen for shortcuts themselves, so the keys below only work while an X11 app is focused.
          {p.state === "failed" && ` Your desktop didn't set up OpsTrellis's shortcuts: ${p.error}.`}
          {p.state === "unavailable" && " Your desktop can't hand shortcuts to apps (GNOME 48 or later and KDE Plasma can)."}
        </div>
      </div>
      {p.state === "failed" && (
        <div className="inline"><button className="btn primary" disabled={busy} onClick={() => run(api.rebindWaylandShortcuts)}>Try again</button></div>
      )}
      {ws.gnome ? (
        <Row title={ws.gnomeShortcuts ? "GNOME custom shortcuts added" : "Add GNOME custom shortcuts"}
          desc="Uses the keys below for dictation, command mode and paste last. They start and finish with a press each; holding to talk needs the desktop's shortcut support.">
          <button className={"btn" + (ws.gnomeShortcuts ? "" : " primary")} disabled={busy} onClick={() => run(api.addGnomeShortcuts)}>
            {ws.gnomeShortcuts ? "Update" : "Add shortcuts"}
          </button>
        </Row>
      ) : (
        <>
          <div className="row-desc">Add custom shortcuts in your desktop's keyboard settings that run:</div>
          <table className="cmd-table">
            <tbody>
              <tr><td className="mono">opstrellis --toggle</td><td>Start or finish dictation</td></tr>
              <tr><td className="mono">opstrellis --command</td><td>Start command mode on the selection</td></tr>
              <tr><td className="mono">opstrellis --paste-last</td><td>Paste the last transcript</td></tr>
              <tr><td className="mono">opstrellis --cancel</td><td>Cancel the current recording</td></tr>
            </tbody>
          </table>
        </>
      )}
      {error && <div className="error">{error}</div>}
    </div>
  );
}

/**
 * `shortcuts`: reloads when they change (choosing Fn needs Input Monitoring).
 * `withShortcutSetup`: include the Wayland shortcut setup (Settings shows it under Shortcuts instead).
 */
export function PermissionsInfo({ shortcuts, withShortcutSetup = true }: { shortcuts?: string; withShortcutSetup?: boolean }) {
  const [perm, load] = usePermissions(shortcuts);
  if (!perm) return null;

  return (
    <>
      {perm.platform === "macos" && (
        <Row title="Accessibility" desc="Needed to paste text into other apps and to read the selection in command mode.">
          {perm.accessibility
            ? <span className="tag ok">Granted</span>
            : <button className="btn primary" onClick={() => api.requestAccessibility().then(load)}>Grant access</button>}
        </Row>
      )}
      {perm.inputMonitoring !== null && (
        <Row title="Input Monitoring" desc="Needed for a single-key shortcut like 🌐 fn or Right ⌥ to work in every app.">
          {perm.inputMonitoring
            ? <span className="tag ok">Granted</span>
            : <button className="btn primary" onClick={() => api.requestInputMonitoring().then(load)}>Grant access</button>}
        </Row>
      )}
      {perm.fnSystemAction && (
        <Row title="🌐 key conflict"
          desc={`Pressing 🌐 fn also ${perm.fnSystemAction}, and OpsTrellis couldn't switch that off. In System Settings → Keyboard, set “Press 🌐 key to” to “Do Nothing”.`}>
          <button className="btn" onClick={load}>Check again</button>
        </Row>
      )}
      {perm.platform === "linux" && !perm.wayland && (
        <Row title="Global shortcuts" desc="Running on X11: shortcuts work out of the box."><span className="tag ok">Ready</span></Row>
      )}
      {perm.waylandShortcuts && withShortcutSetup && <WaylandShortcutsSetup ws={perm.waylandShortcuts} reload={load} />}
      {perm.insertion && (
        <Row title="Text insertion"
          desc={perm.insertion.ready
            ? `Pastes into Wayland apps with ${perm.insertion.ready}.`
            : `${perm.insertion.problem}, so text can't be pasted into Wayland apps (it's left on your clipboard). ${perm.insertion.fix}`}>
          {perm.insertion.ready ? <span className="tag ok">Ready</span> : <button className="btn" onClick={load}>Check again</button>}
        </Row>
      )}
    </>
  );
}

