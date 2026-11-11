import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import {
  api, errMsg, useEvent, type BackupSummary, type CloudStatus, type SettingsDetail, type LocalModel, type Permissions, type PolishModel, type WaylandShortcuts, type ProviderKind, type Settings, type SttEntry, type TeamStatus, type UpdateInfo,
} from "../api";
import { Icon } from "../icons";
import { WorkspacesSection } from "./Workspaces";
import { IS_MAC, Keys, isSingleKey, Modal, Page, Progress, Row, Section, Toggle, UnencryptedWarning, comboText, useModelDownloads, type SettingsProps } from "../ui";

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
  ["Speech & AI", ["Transcription", "Local models", "AI polish", "Workspaces"]],
  ["Account & privacy", ["Privacy", "Agents", "Backup", "Account", "About"]],
];
const SECTIONS = GROUPS.flatMap(([, sections]) => sections);

type Props = SettingsProps & { saveError: string | null; saveNow: () => Promise<void>; update: UpdateInfo | null };

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
            <Providers {...props} localIds={models.filter((m) => m.installed).map((m) => m.id)} />
          </SettingsSection>

          <SettingsSection title="Local models" desc="Run speech recognition on this computer. Nothing leaves your device.">
            <LocalModels models={models} reload={loadModels} />
          </SettingsSection>

          <SettingsSection title="AI polish" desc="AI polish cleans up dictations, runs transforms and command mode, and summarizes meetings.">
            <LlmSection settings={settings} set={set} saveNow={props.saveNow} />
            <ReviewRewrites settings={settings} set={set} />
          </SettingsSection>

          <WorkspacesSection id={anchor("Workspaces")} settings={settings} set={set} />

          <SettingsSection title="Privacy"><Privacy {...props} /></SettingsSection>
          <SettingsSection title="Agents" desc="Let AI agents like Claude Code or Cursor read what you dictated and your notes, through OpsTrellis as an MCP server."><Agents {...props} /></SettingsSection>
          <SettingsSection title="Backup" desc="Save your dictionary, snippets, actions and style settings to a file, or add them from one. API keys, providers and shortcuts are never included."><Backup {...props} /></SettingsSection>
          <SettingsSection title="Account"><Account {...props} /></SettingsSection>

          <SettingsSection title="About"><About {...props} /></SettingsSection>

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
      // A pending save of a new address would delete a key saved before it.
      await saveNow();
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
                onChange={(e) => {
                  update({ baseUrl: e.target.value || null });
                  // Saving drops the key entered for the old address; show that it's gone.
                  if (hasKey) saveNow().then(() => api.hasSecrets()).then((s) => setHasKey(s.includes("llm")), () => {});
                }} />
              <UnencryptedWarning url={llm.baseUrl} />
              {hasKey && <span className="meta">Changing the address clears the saved key.</span>}
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

/** Command mode: whether a rewrite is shown in the pill before it replaces the selection. */
function ReviewRewrites({ settings, set }: SettingsProps) {
  const [perm] = usePermissions();
  const wayland = !!perm?.wayland;
  return (
    <Row title="Review AI rewrites"
      desc={wayland
        ? "Not available on Wayland, where apps can't use Enter and Esc as shortcuts: rewrites replace the selection right away."
        : "Command mode shows the rewrite in the recording pill before it replaces your selection: press Enter to insert it or Esc to discard it. Long means over 280 characters."
          + (settings.overlay ? "" : " Needs the recording pill (General).")}>
      <select value={wayland ? "never" : settings.reviewRewrites} disabled={wayland} aria-label="Review AI rewrites"
        onChange={(e) => set("reviewRewrites", e.target.value as Settings["reviewRewrites"])}>
        <option value="long">Only long text</option>
        <option value="always">Always</option>
        <option value="never">Never</option>
      </select>
    </Row>
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

function Providers({ settings, set, saveNow, localIds }: Props & { localIds: string[] }) {
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
        <ProviderEntry key={entry.id} entry={entry} index={i} count={chain.length} localIds={localIds}
          hasKey={secrets.includes(`stt:${entry.id}`)} onKeySaved={loadSecrets} saveNow={saveNow}
          update={(p) => update(entry.id, p)} move={(d) => move(i, d)} remove={() => remove(entry.id)}
          primary={chain.findIndex((e) => e.enabled) === i} />
      ))}
      <div className="row"><button className="btn" onClick={add}><Icon name="plus" />Add provider</button></div>
    </>
  );
}

interface EntryProps {
  entry: SttEntry; index: number; count: number; primary: boolean; localIds: string[]; hasKey: boolean;
  update: (p: Partial<SttEntry>) => void; move: (d: number) => void; remove: () => void;
  onKeySaved: () => void; saveNow: () => Promise<void>;
}

function ProviderEntry({ entry, index, count, primary, localIds, hasKey, update, move, remove, onKeySaved, saveNow }: EntryProps) {
  const [key, setKey] = useState("");
  const [test, setTest] = useState<{ busy?: boolean; text?: string; latencyMs?: number; error?: string } | null>(null);
  const suggestions = entry.provider === "local" ? localIds : MODELS[entry.provider];
  const needsKey = entry.provider !== "local";

  async function saveKey() {
    try { await saveNow(); await api.setSecret(`stt:${entry.id}`, key.trim()); setKey(""); onKeySaved(); }
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
            const model = provider === "local" ? localIds[0] ?? "moonshine-tiny-en" : MODELS[provider][0];
            update({ provider, model, streaming: STREAMING.includes(provider) && entry.streaming });
            // Saving drops the key kept for the previous provider; show that it's gone.
            saveNow().then(onKeySaved, () => {});
          }}>
            {Object.entries(PROVIDERS).map(([id, label]) => <option key={id} value={id}>{label}</option>)}
          </select>
        </label>
        <label className="field">
          <span>Model</span>
          <input list={`models-${entry.id}`} value={entry.model} onChange={(e) => update({ model: e.target.value })} />
          <datalist id={`models-${entry.id}`}>{suggestions.map((m) => <option key={m} value={m} />)}</datalist>
        </label>
        {(entry.provider === "openai" || entry.provider === "cloud") && (
          <label className="field span2">
            <span>Base URL <em>{entry.provider === "openai" ? "optional: Groq, Fireworks, self-hosted…" : "optional: self-hosted gateway"}</em></span>
            <input value={entry.baseUrl ?? ""} placeholder={entry.provider === "openai" ? "https://api.openai.com/v1" : "Uses your OpsTrellis Cloud URL"}
              onChange={(e) => {
                update({ baseUrl: e.target.value || null });
                if (hasKey) saveNow().then(onKeySaved, () => {});
              }} />
            <UnencryptedWarning url={entry.baseUrl} />
            {hasKey && <span className="meta">Changing the address clears the saved key.</span>}
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

function LocalModels({ models, reload }: { models: LocalModel[]; reload: () => void }) {
  const progress = useModelDownloads(() => reload());
  const [error, setError] = useState<string | null>(null);
  const failed = models.map((m) => progress[m.id]).find((p) => p?.error);
  const download = (id: string, allowUnverified = false) => { setError(null); api.downloadLocalModel(id, allowUnverified).catch((e) => setError(errMsg(e))); };

  return (
    <>
      {models.map((m) => {
        const p = progress[m.id];
        return (
          <div key={m.id} className="row">
            <div className="row-text">
              <div className="row-title">{m.name} <span className="meta">· {m.languages} · {m.sizeMb} MB</span></div>
              <div className="row-desc">{m.description}</div>
              {p && !p.error && <Progress value={p.total ? p.downloaded / p.total : 0} />}
            </div>
            <div className="row-control">
              {m.installed ? (
                <>
                  <span className="tag ok">Installed</span>
                  <button className="icon-btn" title="Delete model"
                    onClick={() => api.deleteLocalModel(m.id).then(reload, (e) => setError(errMsg(e)))}><Icon name="trash" /></button>
                </>
              ) : (
                <button className="btn" disabled={(!!p && !p.error) || m.downloading}
                  onClick={() => download(m.id)}>
                  {p && !p.error ? `${Math.round((p.downloaded / (p.total || 1)) * 100)}%` : m.downloading ? "Downloading…" : "Download"}
                </button>
              )}
            </div>
          </div>
        );
      })}
      {models.length === 0 && <div className="list-empty">No local models available.</div>}
      {failed?.changed ? (
        <div className="row column">
          <div className="error">{failed.error}</div>
          <div className="inline">
            <button className="btn" onClick={() => download(failed.id, true)}>Download anyway</button>
            <span className="meta">Or update OpsTrellis (Settings → About) to get the file it was checked against.</span>
          </div>
        </div>
      ) : (error || failed) && <div className="error">{error ?? `Download failed: ${failed!.error}`}</div>}
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
      <Row title="Keep recordings" desc={r === 0
        ? "History is off, so recordings stay in memory only: a failed one until you retry it or quit."
        : "A failed recording stays in memory until you retry it or quit; the other choices also save recordings on this computer, deleted within an hour of turning 24 hours old (or at the next launch)."}>
        <select value={settings.privacy.audioRetention}
          onChange={(e) => set("privacy", { ...settings.privacy, audioRetention: e.target.value as Settings["privacy"]["audioRetention"] })}>
          <option value="off">Never</option>
          <option value="failed">Failed ones, for 24 hours</option>
          <option value="all">All, for 24 hours</option>
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
          This deletes every saved dictation and kept recording on this computer. Your Insights totals are kept. This can't be undone.
        </Modal>
      )}
    </>
  );
}

// ---------- Agents ----------

const AGENT_SETUP: [string, string][] = [
  ["Claude Code: run this in a terminal", "claude mcp add opstrellis -- opstrellis mcp"],
  ["Other agents: add an MCP server with", `{ "command": "opstrellis", "args": ["mcp"] }`],
];

function Agents({ settings, set }: SettingsProps) {
  const a = settings.agents;
  const update = (patch: Partial<Settings["agents"]>) => set("agents", { ...a, ...patch });
  const [copied, setCopied] = useState<string | null>(null);
  const copy = (text: string) => navigator.clipboard.writeText(text).then(() => setCopied(text), () => {});

  return (
    <>
      <Row title="Agent access" desc={a.enabled ? "On: agents can use what you allow below." : "Off: agents get nothing from OpsTrellis."}>
        <Toggle checked={a.enabled} onChange={(v) => update({ enabled: v })} label="Agent access" />
      </Row>
      {a.enabled && <>
        <Row title="Dictation history" desc="Agents can search and read your saved dictations, and read your dictionary.">
          <Toggle checked={a.history} onChange={(v) => update({ history: v })} label="Dictation history" />
        </Row>
        <Row title="Notes" desc="Agents can list and read your notes and meeting notes, including what others said in a meeting.">
          <Toggle checked={a.notes} onChange={(v) => update({ notes: v })} label="Notes" />
        </Row>
        <Row title="Add dictionary words" desc="Agents can add technical terms and names for OpsTrellis to spell right, only ones that can't be ordinary words: with a capital inside a word (gRPC, PostgreSQL), a digit (k8s), or + # . _ / (C++, Node.js). They can't add replacements or change or delete entries.">
          <Toggle checked={a.addTerms} onChange={(v) => update({ addTerms: v })} label="Add dictionary words" />
        </Row>
        <Row title="Questions by voice" desc="Lets an agent show a question in the pill. You answer by dictating; OpsTrellis never starts recording by itself.">
          <Toggle checked={a.voice} onChange={(v) => update({ voice: v })} label="Questions by voice" />
        </Row>
      </>}
      <div className="row column">
        {AGENT_SETUP.flatMap(([label, text]) => [
          <div key={label} className="row-desc">{label}:</div>,
          <div key={text} className="code-row">
            <code className="code-line mono">{text}</code>
            <button className="btn" onClick={() => copy(text)}>{copied === text ? "Copied" : "Copy"}</button>
          </div>,
        ])}
        <div className="row-desc">
          {IS_MAC
            ? "Installed from the .dmg? Use /Applications/OpsTrellis.app/Contents/MacOS/opstrellis in place of opstrellis."
            : "Running the AppImage without the installer? Use its full path in place of opstrellis."}
          {" "}Turning a switch off applies to an agent's next request. After turning one on, restart the agent so it sees the new tools.
        </div>
        <div className="row-desc">
          This is your consent for what agents get through OpsTrellis, not a lock: other software running as you can read OpsTrellis's files directly.
        </div>
      </div>
    </>
  );
}

// ---------- Backup ----------

const MAX_FILE_BYTES = 5 * 1024 * 1024;
const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;
const countLine = (c: { add: number; skip: number }, one: string, many: string) =>
  `${plural(c.add, "new " + one, "new " + many)}${c.skip ? `, ${c.skip} already there` : ""}`;

const TONE_NAMES: Record<string, string> = { formal: "formal", casual: "casual", very_casual: "very casual", excited: "excited" };

/** One line per kind of setting the file would replace. */
function settingsLines(d: SettingsDetail): string[] {
  const pairs = (o: Record<string, string>) => Object.entries(o).map(([k, v]) => `${k} → ${TONE_NAMES[v] ?? v}`).join(", ");
  return [
    d.language && `Language: ${d.language}`,
    d.promptStyle && `AI prompt style: ${d.promptStyle}`,
    Object.keys(d.styles).length > 0 && `Tones: ${pairs(d.styles)}`,
    d.styleNotes > 0 && plural(d.styleNotes, "style note", "style notes"),
    d.cleanup > 0 && plural(d.cleanup, "cleanup switch", "cleanup switches"),
    d.termPacks && `Built-in term packs: ${d.termPacks.length ? d.termPacks.join(", ") : "none"}`,
    Object.keys(d.outputLanguages).length > 0 && `Write in: ${pairs(d.outputLanguages)}`,
    d.reviewRewrites && `Review rewrites: ${d.reviewRewrites}`,
    d.appRules > 0 && plural(d.appRules, "app rule", "app rules"),
    Object.keys(d.autoActions).length > 0 && `Run after every dictation: ${pairs(d.autoActions)}`,
  ].filter((l): l is string => !!l);
}

function Backup({ set, saveNow }: SettingsProps & { saveNow: () => Promise<void> }) {
  const fileInput = useRef<HTMLInputElement>(null);
  const [includeNotes, setIncludeNotes] = useState(false);
  const [saved, setSaved] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [pending, setPending] = useState<{ json: string; summary: BackupSummary } | null>(null);
  const [withSettings, setWithSettings] = useState(false);
  const [withNotes, setWithNotes] = useState(false);

  async function exportFile() {
    setBusy(true);
    setError(null);
    setResult(null);
    try {
      const r = await api.exportBackup(includeNotes);
      const parts = [plural(r.dictionary, "dictionary entry", "dictionary entries"), plural(r.snippets, "snippet", "snippets"), plural(r.actions, "action", "actions")];
      if (includeNotes) parts.push(plural(r.notes, "note", "notes"));
      setSaved(`Saved ${parts.join(", ")} to ${r.path}.${r.skipped ? ` ${plural(r.skipped, "entry was", "entries were")} left out because ${r.skipped === 1 ? "it" : "they"} couldn't be imported again (too long, with a control character, or beyond the dictionary's size limit).` : ""}`);
    } catch (e) { setError(errMsg(e)); }
    setBusy(false);
  }

  async function pick(file: File | undefined) {
    if (!file) return;
    setError(null);
    setResult(null);
    if (file.size > MAX_FILE_BYTES) return setError("That file is over 5 MB, too big to be an OpsTrellis backup.");
    try {
      const json = await file.text();
      setPending({ json, summary: await api.previewBackup(json) });
      setWithSettings(false);
      setWithNotes(false);
    } catch (e) { setError(errMsg(e)); }
  }

  async function importFile() {
    if (!pending) return;
    setBusy(true);
    try {
      if (withSettings) await saveNow(); // a save still in flight would send the old values back over the restored ones
      const r = await api.importBackup(pending.json, withSettings, withNotes);
      const added = [
        r.dictionary.add && plural(r.dictionary.add, "dictionary entry", "dictionary entries"), r.snippets.add && plural(r.snippets.add, "snippet", "snippets"),
        r.actions.add && plural(r.actions.add, "action", "actions"), r.notes && plural(r.notes, "note", "notes"),
      ].filter(Boolean);
      const already = r.dictionary.skip + r.snippets.skip + r.actions.skip;
      setResult(`${added.length ? "Added " + added.join(", ") : "Nothing new to add"}.${already ? ` ${already} already there.` : ""}${r.hasSettings ? " Style and cleanup settings restored." : ""}`);
      if (r.hasSettings) {
        // The backend already saved them; take them into this window so a later edit here doesn't overwrite them.
        const s = await api.getSettings();
        set("language", s.language);
        set("cleanup", s.cleanup);
        set("styles", s.styles);
        set("outputLanguages", s.outputLanguages);
        set("reviewRewrites", s.reviewRewrites);
        set("styleNotes", s.styleNotes);
        set("promptStyle", s.promptStyle);
        set("appCategories", s.appCategories);
        set("autoTransforms", s.autoTransforms);
      }
      setPending(null);
    } catch (e) { setError(errMsg(e)); setPending(null); }
    setBusy(false);
  }

  const nothing = pending && !withSettings && !withNotes
    && pending.summary.dictionary.add + pending.summary.snippets.add + pending.summary.actions.add === 0;

  return (
    <>
      <Row title="Export backup" desc={saved ?? "Saves a file to your Downloads folder."}>
        <label className="check">
          <input type="checkbox" checked={includeNotes} onChange={(e) => setIncludeNotes(e.target.checked)} />Include notes and meetings
        </label>
        <button className="btn" disabled={busy} onClick={exportFile}>Export backup</button>
      </Row>
      <Row title="Import backup" desc={result ?? "Adds what's in the file. Entries you already have are kept. Settings you choose to restore replace the current ones."}>
        <button className="btn" disabled={busy} onClick={() => fileInput.current?.click()}>Import backup…</button>
        <input ref={fileInput} type="file" accept=".json,application/json" hidden
          onChange={(e) => { void pick(e.target.files?.[0]); e.target.value = ""; }} />
      </Row>
      {error && <div className="error">{error}</div>}
      {pending && (
        <Modal title="Import backup?" onClose={() => setPending(null)}
          footer={<>
            <button className="btn" onClick={() => setPending(null)}>Cancel</button>
            <button className="btn primary" disabled={busy || !!nothing} onClick={importFile}>Import</button>
          </>}>
          <div>
            <div>{countLine(pending.summary.dictionary, "dictionary entry", "dictionary entries")}</div>
            <div>{countLine(pending.summary.snippets, "snippet", "snippets")}</div>
            <div>{countLine(pending.summary.actions, "action", "actions")}</div>
            {pending.summary.notes > 0 && <div>{plural(pending.summary.notes, "new note or meeting", "new notes and meetings")}</div>}
          </div>
          {pending.summary.settings && (
            <div>
              <label className="check">
                <input type="checkbox" checked={withSettings} onChange={(e) => setWithSettings(e.target.checked)} />Also restore these settings, replacing the current ones
              </label>
              <div className="meta">
                {settingsLines(pending.summary.settings).map((l) => <div key={l}>{l}</div>)}
                <div>App rules that mark an app as “AI prompts” are never restored. Actions to run after every dictation are only taken from the actions you already have.</div>
              </div>
            </div>
          )}
          {pending.summary.notes > 0 && (
            <label className="check">
              <input type="checkbox" checked={withNotes} onChange={(e) => setWithNotes(e.target.checked)} />Import notes and meetings
            </label>
          )}
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
      {status?.signedIn && <TeamLibrary settings={settings} set={set} />}
      <Row title="Server URL" desc={status?.signedIn ? "Sign out to change the server." : "Change this only if you run your own OpsTrellis Cloud."}>
        <input className="wide" value={settings.cloud.baseUrl} readOnly={!!status?.signedIn}
          onChange={(e) => set("cloud", { ...settings.cloud, baseUrl: e.target.value })} />
      </Row>
      <UnencryptedWarning url={settings.cloud.baseUrl} />
      {error && <div className="error">{error}</div>}
    </>
  );
}

const count = (n: number, one: string) => `${n} ${one}${n === 1 ? "" : "s"}`;

function synced(iso: string | null) {
  if (!iso) return "not synced yet";
  const min = Math.floor((Date.now() - new Date(iso).getTime()) / 60e3);
  if (min < 1) return "synced just now";
  if (min < 60) return `synced ${min} min ago`;
  const h = Math.floor(min / 60);
  return h < 24 ? `synced ${h} h ago` : `synced ${Math.floor(h / 24)} d ago`;
}

/** The team's shared dictionary, snippets and actions, synced down read-only. */
function TeamLibrary({ settings, set }: SettingsProps) {
  const [team, setTeam] = useState<TeamStatus | null>(null);
  const [syncing, setSyncing] = useState(false);
  const load = () => api.teamStatus().then(setTeam, () => {});
  useEffect(() => {
    void load();
    const t = setInterval(load, 60e3); // keeps "synced … ago" current
    return () => clearInterval(t);
  }, []);
  useEvent("team-updated", () => void load());
  const on = settings.cloud.teamLibrary;

  async function syncNow() {
    setSyncing(true);
    try { setTeam(await api.teamSyncNow()); } catch { /* the error is in the status */ } finally { setSyncing(false); }
  }

  let desc: ReactNode = "Off. Your team's shared words, snippets and actions aren't used.";
  if (on && team) {
    const what = team.team
      ? `${team.team.name}: ${count(team.dictionary, "word")} · ${count(team.snippets, "snippet")} · ${count(team.actions, "action")}${team.skipped ? ` · ${team.skipped} not used` : ""}`
      : team.syncedAt ? "You're not in a team." : "";
    const text = [what, team.error ? "" : synced(team.syncedAt)].filter(Boolean).join(" · ");
    desc = <>{text.charAt(0).toUpperCase() + text.slice(1)}{team.error && <>{text && " · "}<span className="error">Last sync failed: {team.error}</span></>}</>;
  }
  return (
    <Row title="Team library" desc={desc}>
      {on && <button className="btn" onClick={syncNow} disabled={syncing}>{syncing ? "Syncing…" : "Sync now"}</button>}
      <Toggle checked={on} label="Use the team library" onChange={(v) => set("cloud", { ...settings.cloud, teamLibrary: v })} />
    </Row>
  );
}

const humanize = (k: string) => k.replace(/([A-Z])/g, " $1").replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());

// ---------- About ----------

const INSTALL_COMMAND = "curl -fsSL https://raw.githubusercontent.com/mtr1920/opstrellis/main/install.sh | bash";

function About({ settings, set, update: pending }: SettingsProps & { update: UpdateInfo | null }) {
  const [version, setVersion] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [checked, setChecked] = useState<UpdateInfo | null>(null);
  const [updateError, setUpdateError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  useEffect(() => void api.version().then(setVersion, () => {}), []);

  async function check() {
    setChecking(true);
    setUpdateError(null);
    try { setChecked(await api.checkUpdate()); } catch (e) { setChecked(null); setUpdateError(errMsg(e)); }
    setChecking(false);
  }
  const update = checked ?? pending; // a newer version the daily check already found shows without a click
  const copy = () => navigator.clipboard.writeText(INSTALL_COMMAND).then(() => setCopied(true), () => {});

  return (
    <>
      <Row title="Version" desc="MIT licensed: free for anyone, including commercial use."><span className="meta mono">{version ?? "…"}</span></Row>
      <Row title="Check for updates"
        desc={updateError ? <span className="error">{updateError}</span>
          : update ? (update.newer ? `Version ${update.latest.replace(/^v/, "")} is available.` : "You're on the latest version.")
          : "Looks up the latest version on github.com. Nothing is installed."}>
        {update?.newer && <button className="btn primary" onClick={() => api.openReleasePage(update.latest).catch((e) => setUpdateError(errMsg(e)))}>Open release page</button>}
        <button className="btn" disabled={checking} onClick={check}>{checking ? "Checking…" : "Check for updates"}</button>
      </Row>
      {update?.newer && (
        <div className="row column">
          <div className="row-desc">To update, run this in a terminal:</div>
          <div className="code-row">
            <code className="code-line mono">{INSTALL_COMMAND}</code>
            <button className="btn" onClick={copy}>{copied ? "Copied" : "Copy"}</button>
          </div>
        </div>
      )}
      <Row title="Check for updates automatically" desc="Once a day, OpsTrellis asks github.com for the latest version number. Nothing about you is sent.">
        <Toggle checked={settings.checkUpdates} onChange={(v) => set("checkUpdates", v)} label="Check for updates automatically" />
      </Row>
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

