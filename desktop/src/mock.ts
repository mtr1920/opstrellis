// Tiny in-memory backend used only when running in a plain browser (no Tauri).
// It lets `npm run dev` render every screen with plausible data.
import type { DictEntry, HistoryItem, LocalModel, Note, PolishModel, Settings, Snippet, Transform } from "./api";

type Cb = (p: unknown) => void;
const listeners = new Map<string, Set<Cb>>();
export function mockListen(event: string, cb: Cb) {
  if (!listeners.has(event)) listeners.set(event, new Set());
  listeners.get(event)!.add(cb);
  return () => void listeners.get(event)!.delete(cb);
}
const emit = (event: string, payload: unknown = null) => listeners.get(event)?.forEach((cb) => cb(payload));

const now = Date.now();
const iso = (msAgo: number) => new Date(now - msAgo).toISOString();
const H = 3600e3, D = 24 * H;
let nextId = 100;

const settings: Settings = {
  hotkeys: { dictate: "Ctrl+Shift+Space", command: "Ctrl+Shift+E", pasteLast: "Ctrl+Shift+V" },
  microphone: null,
  language: "auto",
  sttChain: [
    { id: "a1", provider: "deepgram", model: "nova-3", streaming: true, enabled: true },
    { id: "b2", provider: "local", model: "moonshine-tiny-en", streaming: false, enabled: true },
  ],
  llm: { enabled: true, provider: "openai", baseUrl: null, model: "gpt-4o-mini" },
  cleanup: { removeFillers: true, voiceCommands: true, autoLearn: false, techTerms: true },
  autoTransforms: {},
  appCategories: { "my-crm": "emails" },
  styleNotes: { email: "Sign off with “Best, Alex”." },
  promptStyle: "clean",
  styles: { personal: "casual", work: "formal", email: "formal", ai: "formal", other: "casual" },
  insertion: { method: "paste", restoreClipboard: true, pasteDelayMs: 50 },
  privacy: { historyRetentionDays: null, syncUsage: true },
  cloud: { baseUrl: "https://cloud.opstrellis.app", email: null },
  overlay: true, sounds: true, launchAtLogin: false, showInDock: true, theme: "system",
  onboarded: !location.search.includes("onboarding"),
};
const secrets = new Set(["stt:a1"]);

const h = (id: number, ago: number, text: string, appName: string, category: string, extra: Partial<HistoryItem> = {}): HistoryItem => ({
  id, createdAt: iso(ago), rawText: text, text, appName, category, provider: "deepgram", model: "nova-3",
  audioMs: 4000, latencyMs: 420, words: text.split(/\s+/).length, status: "ok", error: null, mode: "dictate", ...extra,
});
let history: HistoryItem[] = [
  h(1, 0.2 * H, "Can you take a look at the pull request before lunch? I left a couple of questions on the parser changes.", "Slack", "work_messages"),
  h(2, 1.1 * H, "Write a Python function that parses ISO dates and returns the weekday name.", "Cursor", "ai_prompts"),
  h(3, 2.5 * H, "", "Firefox", "other", { status: "cancelled" }),
  h(4, 3 * H, "Hi Maria, thanks for sending the contract over. I'll review it tonight and get back to you tomorrow morning.", "Thunderbird", "emails"),
  h(5, 26 * H, "Sounds good, see you at 7", "Signal", "personal_messages", { rawText: "um sounds good see you at seven" }),
  h(6, 28 * H, "", "Terminal", "other", { status: "error", error: "Deepgram: 401 Unauthorized, fell back failed" }),
  h(7, 3 * D, "The quarterly report shows a twelve percent increase in active users, driven mostly by the new onboarding flow.", "LibreOffice Writer", "documents"),
];
const days = Array.from({ length: 180 }, (_, i) => {
  const date = new Date(now - i * D).toISOString().slice(0, 10);
  const words = (i * 7919) % 11 < 4 ? 0 : ((i * 104729) % 900) + 40;
  return { date, words };
}).filter((d) => d.words > 0);

let dictionary: DictEntry[] = [
  { id: 1, phrase: "Kubernetes", replacement: null, createdAt: iso(5 * D), learned: false },
  { id: 2, phrase: "OpsTrellis", replacement: null, createdAt: iso(5 * D), learned: false },
  { id: 3, phrase: "my email", replacement: "alex@example.com", createdAt: iso(2 * D), learned: false },
];
let snippets: Snippet[] = [
  { id: 1, trigger: "my address", expansion: "221B Baker Street, London NW1 6XE", createdAt: iso(4 * D) },
  { id: 2, trigger: "calendar link", expansion: "https://cal.example.com/alex/30min", createdAt: iso(1 * D) },
];
let transforms: Transform[] = [
  { id: 1, name: "Make formal", prompt: "Rewrite the text in a formal, professional tone.", builtin: true, output: "replace" },
  { id: 2, name: "Bullet points", prompt: "Turn the text into a concise bulleted list.", builtin: true, output: "replace" },
  { id: 3, name: "Reply", prompt: "Write a reply to the selected message.", builtin: true, output: "copy" },
  { id: 4, name: "Summarize", prompt: "Summarize the selected text in a few short bullet points.", builtin: true, output: "copy" },
  { id: 5, name: "Pirate", prompt: "Rewrite the text as a pirate would say it.", builtin: false, output: "replace" },
];
let notes: Note[] = [
  { id: 1, title: "Talk outline", body: "Intro: why voice beats typing\nDemo: dictation in three apps\nQ&A", updatedAt: iso(H), kind: "note" },
  { id: 2, title: "Groceries", body: "oat milk, coffee beans, lemons", updatedAt: iso(2 * D), kind: "note" },
  { id: 3, title: "Meeting · Oct 14, 10:00", body: "## Summary\nWeekly sync on the launch.\n\n## Action items\n- [ ] Sam: send the pricing draft\n\n## Transcript (12 min)\nOk let's start…", updatedAt: iso(D), kind: "meeting" },
];
const polishModel: PolishModel = { name: "S1-mini by Superwhisper", sizeMb: 484, installed: false, downloading: false };
const EU = ["bg","hr","cs","da","nl","en","et","fi","fr","de","el","hu","it","lv","lt","mt","pl","pt","ro","sk","sl","es","sv","ru","uk"];
const WHISPER = ["en","zh","de","es","ru","ko","fr","ja","pt","tr","pl","ca","nl","ar","sv","it","id","hi","fi","vi","he","uk","el","ms","cs","ro","da","hu","ta","no","th","ur"];
const m = (id: string, name: string, languages: string, sizeMb: number, langs: string[], recommended: boolean, description: string, installed = false): LocalModel =>
  ({ id, name, languages, sizeMb, langs, recommended, description, installed, downloading: false });
const models: LocalModel[] = [
  m("moonshine-tiny-en", "Moonshine Tiny", "English", 103, ["en"], false, "Fastest. Good for short English dictation on any CPU.", true),
  m("moonshine-base-en", "Moonshine Base", "English", 251, ["en"], false, "More accurate English, still faster than real time on laptops."),
  m("parakeet-v2-en", "Parakeet TDT 0.6B v2", "English", 482, ["en"], false, "Very accurate English with punctuation and capitalization."),
  m("parakeet-unified-en", "Parakeet Unified 0.6B", "English", 501, ["en"], true, "Newest NVIDIA English model (2026): best English accuracy, with punctuation and capitalization."),
  m("parakeet-v3", "Parakeet TDT 0.6B v3", "25 European languages", 487, EU, false, "Multilingual European model with automatic language detection."),
  m("parakeet-ultra", "Parakeet Ultra 0.6B", "25 European languages", 487, EU, true, "Moondream's retrained Parakeet v3 (2026): more accurate than v3 in every language tested."),
  m("whisper-base", "Whisper Base", "99 languages", 208, WHISPER, false, "Multilingual Whisper, small and quick."),
  m("whisper-small", "Whisper Small", "99 languages", 639, WHISPER, false, "Multilingual Whisper with better accuracy."),
  m("whisper-turbo", "Whisper Large v3 Turbo", "99 languages", 564, WHISPER, true, "Highest multilingual accuracy; needs a fast CPU."),
  m("sense-voice", "SenseVoice Small", "Chinese, English, Japanese, Korean, Cantonese", 163, ["zh", "en", "ja", "ko", "yue"], true, "Very fast multilingual model for East Asian languages."),
];
const downloads = new Map<string, { timer: ReturnType<typeof setInterval>; finish: () => void }>();

function upsert<T extends { id: number }>(list: T[], item: Partial<T>, fill: Partial<T>): [T[], T] {
  const existing = list.find((x) => x.id === item.id);
  const next = { ...fill, ...existing, ...item, id: existing?.id ?? nextId++ } as T;
  return [existing ? list.map((x) => (x.id === next.id ? next : x)) : [next, ...list], next];
}

const handlers: Record<string, (a: any) => unknown> = {
  get_settings: () => structuredClone(settings),
  save_settings: ({ settings: s }) => {
    for (const k of Object.values(s.hotkeys) as string[]) if (!/\+?[A-Za-z0-9]+$/.test(k) || k.endsWith("+")) throw `Invalid hotkey "${k}"`;
    Object.assign(settings, s);
    return structuredClone(settings);
  },
  set_secret: ({ id }) => void secrets.add(id),
  delete_secret: ({ id }) => void secrets.delete(id),
  has_secrets: () => [...secrets],
  list_input_devices: () => ["Built-in Microphone", "USB Audio Device", "Headset (Bluetooth)"],
  list_history: ({ query }) => history.filter((x) => !query || x.text.toLowerCase().includes(query.toLowerCase())),
  update_history: ({ id, text }) => { history = history.map((x) => (x.id === id ? { ...x, text } : x)); return { added: [], pending: [], undo: [] }; },
  undo_learned: () => {},
  repolish_history: ({ id }) => {
    const item = history.find((x) => x.id === id)!;
    const text = item.rawText.charAt(0).toUpperCase() + item.rawText.slice(1);
    history = history.map((x) => (x.id === id ? { ...x, text } : x));
    return new Promise((r) => setTimeout(() => r(text), 500));
  },
  delete_history: ({ id }) => { history = history.filter((x) => x.id !== id); emit("history-updated"); },
  clear_history: () => { history = []; emit("history-updated"); },
  list_apps: () => [
    { appName: "Slack", category: "work_messages", count: 48 }, { appName: "Cursor", category: "ai_prompts", count: 31 },
    { appName: "Thunderbird", category: "emails", count: 12 }, { appName: "Firefox", category: "other", count: 9 },
  ],
  get_stats: () => ({
    totalWords: 48210, totalDictations: 1312, wpm: 142, fixes: 318, streak: 9, longestStreak: 23, days,
    categories: [
      { category: "ai_prompts", count: 420, words: 15800 }, { category: "work_messages", count: 380, words: 12100 },
      { category: "emails", count: 160, words: 9800 }, { category: "documents", count: 90, words: 6400 },
      { category: "personal_messages", count: 210, words: 3100 }, { category: "other", count: 52, words: 1010 },
    ],
  }),
  list_dictionary: () => dictionary,
  upsert_dictionary: ({ entry }) => { let e; [dictionary, e] = upsert(dictionary, entry, { createdAt: iso(0) }); return e; },
  delete_dictionary: ({ id }) => void (dictionary = dictionary.filter((x) => x.id !== id)),
  list_snippets: () => snippets,
  upsert_snippet: ({ snippet }) => { let e; [snippets, e] = upsert(snippets, snippet, { createdAt: iso(0) }); return e; },
  delete_snippet: ({ id }) => void (snippets = snippets.filter((x) => x.id !== id)),
  list_transforms: () => transforms,
  upsert_transform: ({ transform }) => { let e; [transforms, e] = upsert(transforms, transform, { builtin: false, output: "replace" }); return e; },
  delete_transform: ({ id }) => void (transforms = transforms.filter((x) => x.id !== id || x.builtin)),
  list_notes: ({ kind }) => notes.filter((n) => !kind || n.kind === kind),
  meeting_start: () => null,
  meeting_templates: () => [{ id: "general", name: "General" }, { id: "sales", name: "Sales call" }, { id: "interview", name: "Interview" }, { id: "one_on_one", name: "1:1" }, { id: "standup", name: "Standup" }],
  meeting_follow_up: ({ kind }) => new Promise((r) => setTimeout(() => r(kind === "crm"
    ? "Summary: Weekly launch sync\nNeeds: Pricing draft\nObjections: Not discussed\nDecision process: Not discussed\nNext step: Sam sends pricing\nNext step date: Friday"
    : "Hi all,\n\nThanks for the sync today. Quick recap:\n- Launch stays on track\n- Sam sends the pricing draft by Friday\n\nBest,\nAlex"), 700)),
  meeting_stop: () => null,
  meeting_status: () => ({ active: false, finishing: false, startedAt: null, transcript: "" }),
  upsert_note: ({ note }) => { let e; [notes, e] = upsert(notes, note, { updatedAt: iso(0), kind: "note" }); e.updatedAt = iso(0); return e; },
  delete_note: ({ id }) => void (notes = notes.filter((x) => x.id !== id)),
  list_local_models: () => structuredClone(models),
  download_local_model: ({ id }) => new Promise<void>((finish) => {
    const m = models.find((x) => x.id === id)!;
    const total = m.sizeMb * 1e6;
    let downloaded = 0;
    m.downloading = true;
    const timer = setInterval(() => {
      downloaded = Math.min(total, downloaded + total / 40);
      if (downloaded < total) return emit("model-download", { id, downloaded, total, done: false, error: null });
      clearInterval(timer); downloads.delete(id); m.installed = true; m.downloading = false;
      emit("model-download", { id, downloaded: 0, total: 0, done: true, error: null, cancelled: false });
      finish();
    }, 250);
    downloads.set(id, { timer, finish });
  }),
  cancel_local_download: ({ id }) => {
    const d = downloads.get(id);
    if (!d) return;
    clearInterval(d.timer);
    downloads.delete(id);
    models.find((x) => x.id === id)!.downloading = false;
    emit("model-download", { id, downloaded: 0, total: 0, done: true, error: null, cancelled: true });
    d.finish();
  },
  polish_model_status: () => ({ ...polishModel }),
  download_polish_model: () => {
    const total = polishModel.sizeMb * 1e6;
    let downloaded = 0;
    polishModel.downloading = true;
    const t = setInterval(() => {
      downloaded = Math.min(total, downloaded + total / 12);
      const done = downloaded >= total;
      if (done) { clearInterval(t); polishModel.installed = true; polishModel.downloading = false; }
      emit("model-download", { id: "s1-mini", downloaded, total, done, error: null });
    }, 250);
  },
  delete_polish_model: () => void (polishModel.installed = false),
  delete_local_model: ({ id }) => void (models.find((x) => x.id === id)!.installed = false),
  test_llm: () => new Promise((r) => setTimeout(() => r({ text: "So let's meet at 3pm on Friday", latencyMs: 640 }), 600)),
  test_stt: () => new Promise((r) => setTimeout(() => r({ text: "The quick brown fox jumps over the lazy dog.", latencyMs: 412 }), 700)),
  start_dictation: () => emit("dictation-state", { status: "recording", mode: "dictate", handsFree: true, stage: "", micReady: true }),
  stop_dictation: () => {
    emit("dictation-state", { status: "processing", mode: "dictate", handsFree: false, stage: "transcribing", micReady: true });
    setTimeout(() => {
      emit("dictation-result", { text: "Hello from the OpsTrellis dev mock.", latencyMs: 380 });
      emit("dictation-outcome", { kind: "ok", message: "Inserted · 0.4 s" });
      emit("dictation-state", { status: "idle", mode: "dictate", handsFree: false, stage: "", micReady: false });
    }, 800);
  },
  cancel_dictation: () => emit("dictation-state", { status: "idle", mode: "dictate", handsFree: false, stage: "", micReady: false }),
  paste_last: () => undefined,
  cloud_login: () => {
    setTimeout(() => emit("cloud-status", { signedIn: true, email: "alex@example.com", plan: "free", usage: { minutesUsed: 4.2, minutesLimit: 15 } }), 4000);
    return { userCode: "WXYZ-1234", verificationUrl: "https://cloud.opstrellis.app/device?code=WXYZ-1234" };
  },
  cloud_logout: () => ({ signedIn: false, email: null, plan: null, usage: null }),
  cloud_status: () => ({ signedIn: false, email: null, plan: null, usage: null }),
  permissions: () => ({
    accessibility: true, platform: "linux", wayland: true, hotkeyError: null, inputMonitoring: null, fnSystemAction: null,
    waylandShortcuts: { portal: { state: "bound", shortcuts: [{ id: "dictate", trigger: "Shift+Ctrl+Space" }, { id: "command", trigger: "Ctrl+Alt+Space" }, { id: "paste-last", trigger: "" }] }, gnome: true, gnomeShortcuts: false },
    insertion: { ready: null, problem: "ydotool isn't installed", fix: "GNOME doesn't support wtype. Install ydotool (sudo apt install ydotool), add yourself to the input group (sudo usermod -aG input $USER), then restart your computer." },
  }),
  rebind_wayland_shortcuts: () => undefined,
  add_gnome_shortcuts: () => undefined,
  request_accessibility: () => undefined,
  request_input_monitoring: () => undefined,
  set_shortcut_recording: () => undefined,
  open_logs: () => undefined,
};

export async function mockInvoke(cmd: string, args: Record<string, unknown> = {}): Promise<unknown> {
  const fn = handlers[cmd];
  if (!fn) throw `mock: unknown command ${cmd}`;
  return fn(args);
}

// Overlay demo: animate the pill so it can be previewed in a browser.
if (location.hash === "#overlay") {
  const words = "so I was thinking we could move the standup to ten tomorrow".split(" ");
  let i = 0;
  setTimeout(() => emit("dictation-state", { status: "recording", mode: "dictate", handsFree: true, stage: "", micReady: true }), 50);
  setInterval(() => emit("audio-level", 0.15 + Math.abs(Math.sin(i++ / 3)) * 0.7 * Math.random()), 50);
  setInterval(() => emit("partial-transcript", words.slice(0, (i / 6) % words.length + 1).join(" ")), 300);
}
