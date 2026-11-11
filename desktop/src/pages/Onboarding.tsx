import { useEffect, useState } from "react";
import { api, errMsg, useEvent, type LocalModel, type ProviderKind, type SttEntry } from "../api";
import { Icon } from "../icons";
import { Keys, Progress, useModelDownloads, type SettingsProps } from "../ui";
import { MODELS, PROVIDERS, PermissionsInfo } from "./Settings";

const DEFAULT_LOCAL = "moonshine-tiny-en";
const KEY_PROVIDERS: Exclude<ProviderKind, "local" | "cloud">[] = ["openai", "deepgram", "assemblyai", "elevenlabs", "google", "azure"];

type Props = SettingsProps & { saveNow: () => Promise<void>; onClose: () => void };

export default function Onboarding({ settings, set, saveNow, onClose }: Props) {
  const [step, setStep] = useState(0);
  const [ready, setReady] = useState(false); // a provider has been configured in step 2

  const [finishError, setFinishError] = useState<string | null>(null);
  async function finish() {
    set("onboarded", true);
    try {
      await saveNow();
      onClose();
    } catch (e) {
      setFinishError(errMsg(e));
    }
  }

  const steps = ["Welcome", "Speech provider", "Try it"];
  return (
    <div className="modal-backdrop">
      <div className="modal onboarding" role="dialog" aria-label="Set up OpsTrellis">
        <div className="modal-head">
          <div className="steps">
            {steps.map((s, i) => (
              <span key={s} className={"step" + (i === step ? " active" : i < step ? " done" : "")}>
                <span className="step-num">{i < step ? <Icon name="check" size={12} /> : i + 1}</span>{s}
              </span>
            ))}
          </div>
          <button className="icon-btn" onClick={onClose} aria-label="Close"><Icon name="x" /></button>
        </div>
        <div className="modal-body">
          {step === 0 && (
            <>
              <h2 className="onb-title">Talk, and your words appear wherever you type.</h2>
              <p className="onb-text">Hold <Keys combo={settings.hotkeys.dictate} /> in any app, speak naturally, and let go.
                OpsTrellis transcribes, tidies up filler words and punctuation, and pastes the result.</p>
              <div className="card"><PermissionsInfo /></div>
            </>
          )}
          {step === 1 && <ChooseProvider settings={settings} set={set} onReady={() => setReady(true)} />}
          {step === 2 && <TryIt hotkey={settings.hotkeys.dictate} command={settings.hotkeys.command} ai={settings.llm.enabled} />}
        </div>
        {finishError && <div className="error">Couldn't save your setup: {finishError}</div>}
        <div className="modal-foot">
          {step > 0 && <button className="btn" onClick={() => setStep(step - 1)}>Back</button>}
          <span className="grow" />
          {step === 1 && !ready && <button className="btn ghost" onClick={() => setStep(2)}>Skip for now</button>}
          {step < 2
            ? <button className="btn primary" onClick={() => { if (step === 1) saveNow().catch(() => {}); setStep(step + 1); }}
                disabled={step === 1 && !ready}>Continue</button>
            : <button className="btn primary" onClick={finish}>Finish</button>}
        </div>
      </div>
    </div>
  );
}

function ChooseProvider({ settings, set, onReady }: SettingsProps & { onReady: () => void }) {
  const [model, setModel] = useState<LocalModel | null>(null);
  const [provider, setProvider] = useState<(typeof KEY_PROVIDERS)[number]>("openai");
  const [key, setKey] = useState("");
  const [done, setDone] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const loadModel = () => api.listLocalModels().then((ms) => setModel(ms.find((m) => m.id === DEFAULT_LOCAL) ?? null), () => {});
  useEffect(() => void loadModel(), []);
  const progress = useModelDownloads((id, cancelled) => { if (id === DEFAULT_LOCAL && !cancelled) { loadModel(); choose({ provider: "local", model: DEFAULT_LOCAL, streaming: true }); } });
  const p = progress[DEFAULT_LOCAL];

  /** Put the chosen provider first in the chain (reusing a matching entry). */
  function choose(e: Pick<SttEntry, "provider" | "model" | "streaming">, id: string = crypto.randomUUID()) {
    const existing = settings.sttChain.find((x) => x.provider === e.provider && x.model === e.model);
    const entry: SttEntry = existing ? { ...existing, enabled: true } : { id, baseUrl: null, region: null, enabled: true, ...e };
    set("sttChain", [entry, ...settings.sttChain.filter((x) => x.id !== entry.id)]);
    setDone(`${PROVIDERS[e.provider]} is set up.`);
    onReady();
  }

  async function saveKey() {
    const model = MODELS[provider][0];
    // Reuse the entry that choose() will keep, so a corrected key replaces the old one.
    const id = settings.sttChain.find((x) => x.provider === provider && x.model === model)?.id ?? crypto.randomUUID();
    try {
      await api.setSecret(`stt:${id}`, key.trim());
      choose({ provider, model, streaming: ["openai", "deepgram", "assemblyai"].includes(provider) }, id);
      setKey("");
    } catch (e) { setError(errMsg(e)); }
  }

  return (
    <>
      <h2 className="onb-title">How should OpsTrellis hear you?</h2>
      <div className="choice-grid">
        <div className="card choice">
          <h3>Private, on this computer <span className="tag">Recommended</span></h3>
          <p className="meta">{model ? `${model.name} · ${model.languages} · ${model.sizeMb} MB.` : "A small local model."} Free, works offline, nothing leaves your device.</p>
          {model?.installed
            ? <button className="btn primary" onClick={() => choose({ provider: "local", model: DEFAULT_LOCAL, streaming: true })}>Use local model</button>
            : p && !p.error
              ? <Progress value={p.total ? p.downloaded / p.total : 0} />
              : <button className="btn primary" disabled={!model} onClick={() => api.downloadLocalModel(DEFAULT_LOCAL).catch((e) => setError(errMsg(e)))}>Download model</button>}
          {p?.error && <div className="error">{p.error}</div>}
        </div>
        <div className="card choice">
          <h3>Use your own API key</h3>
          <p className="meta">Cloud providers are the most accurate, especially for accents and noisy rooms.</p>
          <select value={provider} onChange={(e) => setProvider(e.target.value as typeof provider)}>
            {KEY_PROVIDERS.map((k) => <option key={k} value={k}>{PROVIDERS[k]}</option>)}
          </select>
          <div className="inline">
            <input type="password" placeholder="API key" aria-label="API key" value={key} onChange={(e) => setKey(e.target.value)} />
            <button className="btn" disabled={!key.trim()} onClick={saveKey}>Save</button>
          </div>
        </div>
      </div>
      {done && <p className="ok"><Icon name="check" />{done} You can add fallbacks later in Settings.</p>}
      {error && <div className="error">{error}</div>}
    </>
  );
}

function TryIt({ hotkey, command, ai }: { hotkey: string; command: string; ai: boolean }) {
  const [result, setResult] = useState<{ text: string; latencyMs: number } | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEvent("dictation-result", (r) => { setResult(r); setError(null); });
  useEvent("dictation-error", setError);

  return (
    <>
      <h2 className="onb-title">Give it a try</h2>
      <p className="onb-text">Click in the box, hold <Keys combo={hotkey} /> and
        say something like <i>“Hey, want to grab lunch tomorrow?”</i>. Let go when you're done.</p>
      <textarea className="try-box" rows={4} placeholder="Your words will appear here…" defaultValue="" />
      {result && <p className="ok"><Icon name="check" />Got it in {result.latencyMs} ms: “{result.text}”</p>}
      {error && <div className="error">{error}</div>}
      {result && (ai
        ? <p className="onb-text">Now select your sentence, hold <Keys combo={command} /> and say <i>“make it more formal”</i>. Command mode rewrites any selected text by voice.</p>
        : <p className="meta">Tip: turn on AI polish in Settings → AI polish to rewrite selected text by voice with <Keys combo={command} />.</p>)}
    </>
  );
}
