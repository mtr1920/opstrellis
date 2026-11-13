// Tiny in-memory backend used only when running in a plain browser (no Tauri).
// It lets `npm run dev` render every screen with plausible data.
import type { DictEntry, HistoryItem, LocalModel, Note, PolishModel, Settings, Snippet, TeamStatus, TermPack, Transform, Workspace } from "./api";

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
  cleanup: { removeFillers: true, voiceCommands: true, autoLearn: false, techTerms: true, termPacks: [] },
  autoTransforms: {},
  appCategories: { "my-crm": "emails" },
  styleNotes: { email: "Sign off with “Best, Alex”." },
  promptStyle: "clean",
  outputLanguages: { work: "en" },
  reviewRewrites: "long",
  styles: { personal: "casual", work: "formal", email: "formal", ai: "formal", other: "casual" },
  insertion: { method: "paste", restoreClipboard: true, pasteDelayMs: 50 },
  privacy: { historyRetentionDays: null, syncUsage: true, audioRetention: "off" },
  cloud: { baseUrl: "https://cloud.opstrellis.app", email: null, teamLibrary: true },
  overlay: true, sounds: true, launchAtLogin: false, showInDock: true, theme: "system",
  onboarded: !location.search.includes("onboarding"),
  checkUpdates: false,
  workspaceReplace: false,
  agents: { enabled: false, history: true, notes: true, addTerms: false, voice: false },
};
const secrets = new Set(["stt:a1"]);

const h = (id: number, ago: number, text: string, appName: string, category: string, extra: Partial<HistoryItem> = {}): HistoryItem => ({
  id, createdAt: iso(ago), rawText: text, text, appName, category, provider: "deepgram", model: "nova-3",
  audioMs: 4000, latencyMs: 420, words: text.split(/\s+/).length, status: "ok", error: null, mode: "dictate", hasAudio: false, ...extra,
});
let history: HistoryItem[] = [
  h(8, 0.1 * H, "", "Slack", "work_messages", { status: "error", error: "All transcription providers failed — Deepgram: timed out", latencyMs: 0, hasAudio: true }),
  h(1, 0.2 * H, "Can you take a look at the pull request before lunch? I left a couple of questions on the parser changes.", "Slack", "work_messages", { hasAudio: true }),
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
  { id: 1, phrase: "Kubernetes", replacement: null, createdAt: iso(5 * D), learned: false, source: "manual" },
  { id: 2, phrase: "OpsTrellis", replacement: null, createdAt: iso(5 * D), learned: false, source: "manual" },
  { id: 3, phrase: "my email", replacement: "alex@example.com", createdAt: iso(2 * D), learned: false, source: "manual" },
  { id: 4, phrase: "Helm", replacement: null, createdAt: iso(1 * D), learned: false, source: "agent" },
  // Synced from the team library (read-only); the user's own "Kubernetes" wins over the second.
  { id: 5, phrase: "Acme Cloud", replacement: null, createdAt: iso(3 * D), learned: false, source: "team", unused: null },
  { id: 6, phrase: "kubernetes", replacement: "K8s", createdAt: iso(3 * D), learned: false, source: "team", unused: "shadowed" },
];
let snippets: Snippet[] = [
  { id: 1, trigger: "my address", expansion: "221B Baker Street, London NW1 6XE", createdAt: iso(4 * D) },
  { id: 2, trigger: "calendar link", expansion: "https://cal.example.com/alex/30min", createdAt: iso(1 * D) },
  { id: 3, trigger: "support hours", expansion: "Our support team is available Monday to Friday, 9:00 to 17:00 CET.", createdAt: iso(3 * D), team: true, unused: null },
];
let transforms: Transform[] = [
  { id: 1, name: "Make formal", prompt: "Rewrite the text in a formal, professional tone.", builtin: true, output: "replace" },
  { id: 2, name: "Bullets", prompt: "Turn the text into a bulleted list, one bullet per sentence.", builtin: true, output: "replace" },
  { id: 3, name: "Reply", prompt: "Write a reply to the selected message.", builtin: true, output: "copy" },
  { id: 4, name: "Summarize", prompt: "Summarize the selected text in a few short bullet points.", builtin: true, output: "copy" },
  { id: 5, name: "Pirate", prompt: "Rewrite the text as a pirate would say it.", builtin: false, output: "replace" },
  { id: 6, name: "Acme voice", prompt: "Rewrite the text in Acme's house style: plain words, short sentences, no exclamation marks.", builtin: false, output: "replace", team: true, unused: null },
];
/** Like the backend's built-in defaults, by row id: "Bullet points" (2) has been edited. */
const builtinDefaults: Record<number, Pick<Transform, "name" | "prompt" | "output">> = {
  1: { name: "Make formal", prompt: "Rewrite the text in a formal, professional tone.", output: "replace" },
  2: { name: "Bullet points", prompt: "Turn the text into a concise bulleted list.", output: "replace" },
  3: { name: "Reply", prompt: "Write a reply to the selected message.", output: "copy" },
  4: { name: "Summarize", prompt: "Summarize the selected text in a few short bullet points.", output: "copy" },
};
const edited = (t: Transform) => {
  const d = builtinDefaults[t.id];
  return !!t.builtin && !!d && (d.name !== t.name || d.prompt !== t.prompt || d.output !== t.output);
};
/** Like `Settings::reset`: the preferences `Settings::default` has; providers, AI service, privacy, cloud and team sync, onboarding and workspaces stay. */
const DEFAULT_PREFERENCES = {
  hotkeys: { dictate: "Ctrl+Shift+Space", command: "Ctrl+Alt+Space", pasteLast: "Ctrl+Alt+V" },
  microphone: null, language: "auto", autoTransforms: {}, appCategories: {}, styleNotes: {}, promptStyle: "clean", outputLanguages: {}, reviewRewrites: "long",
  styles: { personal: "casual", work: "casual", email: "formal", ai: "formal", other: "formal" },
  insertion: { method: "paste", restoreClipboard: true, pasteDelayMs: 250 },
  overlay: true, sounds: true, launchAtLogin: false, showInDock: true, theme: "system", checkUpdates: false, workspaceReplace: false,
  agents: { enabled: false, history: true, notes: true, addTerms: false, voice: false },
} satisfies Partial<Settings>;
let teamSyncedAt = iso(3 * 60e3);
const teamStatus = (): TeamStatus => (settings.cloud.teamLibrary
  ? { team: { id: "t1", name: "Acme" }, dictionary: dictionary.filter((x) => x.source === "team").length, snippets: snippets.filter((x) => x.team).length,
      actions: transforms.filter((x) => x.team).length, skipped: 0, syncedAt: teamSyncedAt, error: null }
  : { team: null, dictionary: 0, snippets: 0, actions: 0, skipped: 0, syncedAt: null, error: null });
/** Like the backend: team entries are read-only here. */
function own<T extends { id: number }>(list: T[], id: number | undefined, team: (x: T) => boolean | undefined) {
  if (list.some((x) => x.id === id && team(x))) throw "Team entries are edited in the team dashboard";
}
let notes: Note[] = [
  { id: 1, title: "Talk outline", body: "Intro: why voice beats typing\nDemo: dictation in three apps\nQ&A", updatedAt: iso(H), kind: "note" },
  { id: 2, title: "Groceries", body: "oat milk, coffee beans, lemons", updatedAt: iso(2 * D), kind: "note" },
  { id: 3, title: "Meeting · Oct 14, 10:00", body: "## Summary\nWeekly sync on the launch.\n\n## Action items\n- [ ] Sam: send the pricing draft\n\n## Transcript (12 min)\nOk let's start…", updatedAt: iso(D), kind: "meeting" },
];
const polishModel = (id: string, name: string, sizeMb: number, kind: PolishModel["kind"], languages: string[], description: string): PolishModel =>
  ({ id, name, sizeMb, kind, languages, license: "Apache 2.0", description, installed: false, downloading: false });
const polishModels: PolishModel[] = [
  polishModel("s1-mini", "S1-mini by Superwhisper", 484, "cleanup", ["en"], "A small model trained only to clean up dictation."),
  polishModel("spokenly-cleanup-1", "Spokenly Cleanup 1", 639, "cleanup", ["en", "de", "es", "fr", "zh", "ru", "ja"], "Cleans up dictation for the app it goes to."),
  polishModel("qwen3.5-4b", "Qwen3.5 4B", 2741, "instruct", [], "Follows instructions: actions, command mode, translation and meeting summaries."),
];
const CANARY = ["en", "de", "es", "fr"];
const COHERE = ["en", "fr", "de", "it", "es", "pt", "el", "nl", "pl", "zh", "ja", "ko", "vi", "ar"];
const EUROPEAN = ["bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt", "mt", "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk"];
const EUROPEAN_NOT_EN_RU = ["bg", "hr", "cs", "da", "nl", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt", "mt", "pl", "pt", "ro", "sk", "sl", "es", "sv", "uk"];
const WHISPER = ["en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv", "it", "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs", "ro", "da", "hu", "ta", "no", "th", "ur", "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn", "et", "mk", "br", "eu", "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si", "km", "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu", "am", "yi", "lo", "uz", "fo", "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg", "as", "tt", "haw", "ln", "ha", "ba", "jv", "su"];
const TURBO_PICKS = ["ko", "ar", "tr", "id", "he", "fa", "th", "ms", "tl", "ca"];
const QWEN3 = ["zh", "en", "yue", "ar", "de", "fr", "es", "pt", "id", "it", "ko", "ru", "th", "vi", "ja", "tr", "hi", "ms", "nl", "sv", "da", "fi", "pl", "cs", "tl", "fa", "el", "hu", "mk", "ro"];
const OMNILINGUAL = ["en", "zh", "yue", "ja", "ko", "vi", "th", "id", "ms", "tl", "ar", "fa", "ur", "hi", "bn", "ta", "te", "mr", "gu", "kn", "ml", "pa", "or", "as", "ne", "sd", "ks", "ps", "tr", "az", "uz", "kk", "ky", "tg", "tk", "mn", "ug", "ru", "uk", "be", "bg", "sr", "hr", "sl", "sk", "cs", "pl", "de", "fr", "es", "pt", "it", "nl", "sv", "da", "no", "fi", "et", "lv", "lt", "hu", "ro", "el", "mt", "is", "ga", "cy", "eu", "ca", "gl", "he", "am", "ha", "yo", "ig", "zu", "xh", "so", "sw", "my", "km", "lo", "si", "jv", "su", "ceb", "tt", "ba", "ka", "hy", "mai", "bho", "brx", "kok", "doi", "mni"];
const INDIAN_NOT_HI = ["bn", "ta", "te", "kn", "ml", "mr", "gu", "pa", "ur", "or", "as", "ne"];
const DOLPHIN = ["zh", "ja", "th", "ru", "ko", "id", "vi", "yue", "hi", "ur", "ms", "uz", "ar", "fa", "bn", "ta", "te", "ug", "gu", "my", "tl", "kk", "or", "ne", "mn", "km", "jv", "lo", "si", "ps", "pa", "kab", "ba", "ks", "tg", "su", "mr", "ky", "az"];
const EAST_ASIAN = ["zh", "en", "ja", "ko", "yue"];
const m = (id: string, name: string, languages: string, sizeMb: number, langs: string[], recommended: string[], description: string, installed = false): LocalModel =>
  ({ id, name, languages, sizeMb, langs, recommended, description, installed, downloading: false });
// Mirrors MODELS in src-tauri/src/stt/local.rs.
const models: LocalModel[] = [
  m("moonshine-tiny-en", "Moonshine Tiny", "English", 103, ["en"], [], "Fastest. Good for short English dictation on any CPU.", true),
  m("moonshine-base-en", "Moonshine Base", "English", 251, ["en"], [], "More accurate English, still faster than real time on laptops."),
  m("parakeet-110m-en", "Parakeet TDT 110M", "English", 108, ["en"], [], "Small, very fast English with punctuation and capitalization."),
  m("parakeet-v2-en", "Parakeet TDT 0.6B v2", "English", 482, ["en"], [], "Very accurate English with punctuation and capitalization."),
  m("parakeet-unified-en", "Parakeet Unified 0.6B", "English", 501, ["en"], ["en"], "Newest NVIDIA English model (2026): best English accuracy, with punctuation and capitalization."),
  m("distil-whisper-v3.5", "Distil-Whisper Large v3.5", "English", 528, ["en"], [], "Whisper Large accuracy on English at a fraction of the cost; robust to accents and noise."),
  m("canary-180m", "Canary 180M Flash", "English, German, Spanish, French", 153, CANARY, [], "Small NVIDIA model with punctuation. Set your spoken language; it uses English otherwise."),
  m("cohere-transcribe", "Cohere Transcribe 2B", "14 languages", 1699, COHERE, [], "Top of the 2026 Open ASR leaderboard. Large and slow on CPU; set your spoken language (English otherwise)."),
  m("parakeet-v3", "Parakeet TDT 0.6B v3", "25 European languages", 487, EUROPEAN, [], "Multilingual European model with automatic language detection."),
  m("parakeet-ultra", "Parakeet Ultra 0.6B", "25 European languages", 487, EUROPEAN, EUROPEAN_NOT_EN_RU, "Moondream's retrained Parakeet v3 (2026): more accurate than v3 in every language tested."),
  m("fastconformer-de", "FastConformer German", "German", 108, ["de"], [], "Small dedicated German model with punctuation."),
  m("fastconformer-es", "FastConformer Spanish", "Spanish", 107, ["es"], [], "Small dedicated Spanish model."),
  m("gigaam-v3-ru", "GigaAM v3", "Russian", 170, ["ru"], ["ru"], "Sber's Russian model with punctuation; preferred over Whisper Large v3 in blind tests."),
  m("whisper-tiny", "Whisper Tiny", "99 languages", 116, WHISPER, [], "Smallest multilingual Whisper: a quick way to try any language."),
  m("whisper-base", "Whisper Base", "99 languages", 208, WHISPER, [], "Multilingual Whisper, small and quick."),
  m("whisper-small", "Whisper Small", "99 languages", 639, WHISPER, [], "Multilingual Whisper with better accuracy."),
  m("whisper-turbo", "Whisper Large v3 Turbo", "99 languages", 564, WHISPER, TURBO_PICKS, "Best speed-to-accuracy of the Whisper models; needs a fast CPU."),
  m("whisper-large-v3", "Whisper Large v3", "99 languages", 1068, WHISPER, [], "The most accurate Whisper, and the slowest: best on a fast desktop CPU."),
  m("qwen3-asr", "Qwen3-ASR 0.6B", "30 languages", 878, QWEN3, ["hi", "zh"], "Alibaba's 2026 model: detects the language and adds punctuation. Slower on CPU."),
  m("omnilingual-300m", "Omnilingual ASR 300M", "1,600+ languages", 292, OMNILINGUAL, [], "Meta's model for over 1,600 languages, including Indian and African languages most models miss."),
  m("omnilingual-1b", "Omnilingual ASR 1B", "1,600+ languages", 787, OMNILINGUAL, INDIAN_NOT_HI, "The larger, more accurate Omnilingual: about 1% character errors on Tamil, Telugu and Kannada in our tests."),
  m("tara", "Tara by Trelis", "Hindi, English", 1043, ["hi", "en"], ["hi"], "Trelis Research's 2026 Hindi model built on Whisper Large v3: state of the art on Hindi benchmarks and strong on English. Hinglish comes out in Devanagari. Slow on CPU."),
  m("dolphin-small", "Dolphin Small", "40 Asian languages", 191, DOLPHIN, [], "Built for Asian languages: Hindi, Bengali, Tamil, Telugu, Marathi, Urdu, Thai and more. No English."),
  m("dolphin-base", "Dolphin Base", "40 Asian languages", 80, DOLPHIN, [], "Smaller, faster Dolphin."),
  m("sense-voice", "SenseVoice Small", "Chinese, English, Japanese, Korean, Cantonese", 163, EAST_ASIAN, ["yue"], "Very fast multilingual model for East Asian languages."),
  m("parakeet-ja", "Parakeet TDT-CTC 0.6B Japanese", "Japanese", 489, ["ja"], ["ja"], "NVIDIA's dedicated Japanese model with punctuation."),
  m("zipformer-vi", "Zipformer Vietnamese", "Vietnamese", 60, ["vi"], ["vi"], "Small dedicated Vietnamese model trained on 70,000 hours."),
];
const downloads = new Map<string, { timer: ReturnType<typeof setInterval>; finish: () => void }>();

function upsert<T extends { id: number }>(list: T[], item: Partial<T>, fill: Partial<T>): [T[], T] {
  const existing = list.find((x) => x.id === item.id);
  const next = { ...fill, ...existing, ...item, id: existing?.id ?? nextId++ } as T;
  return [existing ? list.map((x) => (x.id === next.id ? next : x)) : [next, ...list], next];
}

const UPDATE = { current: "0.3.2", latest: "v0.3.3", newer: true, url: "https://github.com/mtr1920/opstrellis/releases/tag/v0.3.3" };
// In-app update, like the backend: one download at a time, kept until the restart. `?noSelfUpdate`
// acts like a .deb install, `?updateError` fails the download and `?installError` the install.
let updateStage: "idle" | "downloading" | "ready" = "idle";
function downloadUpdate() {
  if (updateStage === "ready") emit("update-download", { state: "ready", percent: 100, error: null });
  if (updateStage !== "idle") return;
  updateStage = "downloading";
  let percent = 0;
  emit("update-download", { state: "downloading", percent: null, error: null });
  const t = setInterval(() => {
    percent += 7;
    if (location.search.includes("updateError") && percent > 40) {
      clearInterval(t);
      updateStage = "idle";
      emit("update-download", { state: "failed", percent: null, error: "Couldn't download the update from github.com: connection reset" });
    } else if (percent >= 100) {
      clearInterval(t);
      updateStage = "ready";
      emit("update-download", { state: "ready", percent: 100, error: null });
    } else emit("update-download", { state: "downloading", percent, error: null });
  }, 200);
}

const BUILTIN_PACKS: TermPack[] = [
  { id: "tech", name: "Tech terms", description: "Developer tools, cloud services and workplace apps.", terms: 304, builtin: true, enabled: true },
  { id: "data-ml", name: "Data and ML", description: "Data science and machine learning: libraries, platforms, models and common abbreviations.", terms: 69, builtin: true, enabled: false },
  { id: "product-design", name: "Product and design", description: "Product, design and marketing tools and terms.", terms: 63, builtin: true, enabled: false },
  { id: "finance", name: "Finance", description: "Finance and accounting terms, metrics and tools.", terms: 67, builtin: true, enabled: false },
];
let userPacks: TermPack[] = [{ id: "medical", name: "Medical", description: "Common drug names.", terms: 120, builtin: false, enabled: false }];
const termPacks = () => [...BUILTIN_PACKS, ...userPacks];
type PackEntry = { term: string; aliases?: string[]; techAliases?: string[] };
function parseTermPack(json: string): { pack: TermPack; entries: PackEntry[] } {
  let p: { id?: unknown; name?: unknown; description?: unknown; terms?: unknown };
  try { p = JSON.parse(json); } catch (e) { throw `This isn't a term pack file: ${e}.`; }
  if (typeof p.id !== "string" || !/^[a-z0-9][a-z0-9-]{0,39}$/.test(p.id)) throw "The pack id must be 1 to 40 lowercase letters, digits or hyphens, starting with a letter or digit.";
  if (BUILTIN_PACKS.some((b) => b.id === p.id)) throw `“${p.id}” is the id of a built-in pack. Give the pack another id.`;
  if (typeof p.name !== "string" || !p.name.trim() || !Array.isArray(p.terms) || !p.terms.length) throw "This isn't a term pack file.";
  const entries = p.terms as PackEntry[];
  const pack: TermPack = { id: p.id, name: p.name.trim(), description: typeof p.description === "string" ? p.description : "", terms: entries.length, builtin: false, enabled: false };
  return { pack, entries };
}

// Workspaces: the backend's checks, and a short fake indexing run.
let workspaces: Workspace[] = [
  { path: "/Users/alex/code/opstrellis", name: "opstrellis", files: 1240, identifiers: 860, indexedAt: iso(2 * 60e3), indexing: false, error: null },
  { path: "/Users/alex/code/old-api", name: "old-api", files: 0, identifiers: 0, indexedAt: iso(H), indexing: false, error: "This folder can't be found. It may have been moved or deleted." },
];
function indexWorkspace(path: string) {
  workspaces = workspaces.map((w) => (w.path === path ? { ...w, indexing: true } : w));
  setTimeout(() => {
    workspaces = workspaces.map((w) => (w.path === path ? { ...w, indexing: false, error: null, files: 312, identifiers: 540, indexedAt: new Date().toISOString() } : w));
    emit("workspaces-changed");
  }, 1200);
  return structuredClone(workspaces);
}
function addWorkspace(raw: string) {
  const path = raw.trim().replace(/(.)\/+$/, "$1");
  if (!path) throw "Enter a folder's path.";
  if (!path.startsWith("/")) throw "Enter the folder's full path, starting with /.";
  if (path === "/" || path === "/Users/alex") throw "Choose a project folder, not the whole disk or your home folder.";
  for (const w of workspaces) {
    if (w.path === path) throw "That folder is already a workspace.";
    if (path.startsWith(w.path + "/") || w.path.startsWith(path + "/")) throw `That folder is inside or contains the workspace ${w.name}.`;
  }
  if (workspaces.length >= 10) throw "You can add up to 10 workspaces. Remove one first.";
  workspaces = [...workspaces, { path, name: path.split("/").pop()!, files: 0, identifiers: 0, indexedAt: null, indexing: true, error: null }];
  return indexWorkspace(path);
}

// Backup import: the backend's merge rule. An existing entry wins; names compare trimmed and case-insensitively.
const key = (s: string) => s.trim().toLowerCase();
function mergeBackup(json: string, write: boolean, withNotes: boolean, withSettings = false) {
  let b: any;
  try { b = JSON.parse(json); } catch { throw "This isn't an OpsTrellis backup (the file isn't valid JSON)."; }
  if (b?.version !== 1) {
    throw typeof b?.version === "number"
      ? `This backup uses format version ${b.version}, which this version of OpsTrellis can't read (it reads version 1). Update OpsTrellis and try again.`
      : "This isn't an OpsTrellis backup (no version number).";
  }
  const merge = <T,>(have: string[], items: any[] | undefined, name: (x: any) => string, make: (x: any) => T) => {
    const seen = new Set(have.map(key));
    const added: T[] = [];
    let skip = 0;
    for (const it of items ?? []) {
      if (seen.has(key(name(it)))) skip++;
      else { seen.add(key(name(it))); added.push(make(it)); }
    }
    return { added, counts: { add: added.length, skip } };
  };
  const d = merge(dictionary.map((x) => x.phrase), b.dictionary, (x) => x.phrase,
    (x): DictEntry => ({ id: nextId++, phrase: x.phrase.trim(), replacement: x.replacement?.trim() || null, createdAt: iso(0), learned: false }));
  const sn = merge(snippets.map((x) => x.trigger), b.snippets, (x) => x.trigger,
    (x): Snippet => ({ id: nextId++, trigger: x.trigger.trim(), expansion: x.expansion, createdAt: iso(0) }));
  const ac = merge(transforms.map((x) => x.name), b.actions, (x) => x.name,
    (x): Transform => ({ id: nextId++, name: x.name.trim(), prompt: x.prompt, builtin: false, output: x.output === "copy" ? "copy" : "replace" }));
  const nt = withNotes
    ? merge(notes.map((x) => x.title + "\n" + x.body), b.notes, (x) => x.title + "\n" + x.body,
      (x): Note => ({ id: nextId++, title: x.title, body: x.body, updatedAt: x.updatedAt ?? iso(0), kind: x.kind === "meeting" ? "meeting" : "note" }))
    : { added: [] as Note[], counts: { add: 0 } };
  const ruleOk = ([, c]: [string, unknown]) => c !== "ai_prompts"; // those rules are never imported
  const rules = Object.entries(b.settings?.appCategories ?? {}).filter(ruleOk);
  const autoActions = Object.fromEntries(Object.entries((b.settings?.autoActions ?? {}) as Record<string, string>)
    .filter(([, n]) => transforms.some((t) => key(t.name) === key(n)))); // only actions that exist before the import
  const builtinIds = BUILTIN_PACKS.filter((p) => p.id !== "tech").map((p) => p.id);
  const packs: string[] | null = Array.isArray(b.settings?.cleanup?.termPacks)
    ? [...new Set<string>(b.settings.cleanup.termPacks.filter((id: unknown) => builtinIds.includes(id as string)))] : null;
  const review = ["long", "always", "never"].includes(b.settings?.reviewRewrites) ? b.settings.reviewRewrites : null;
  const languages = Object.fromEntries(Object.entries((b.settings?.outputLanguages ?? {}) as Record<string, string>).filter(([, v]) => typeof v === "string"));
  const detail = b.settings && {
    language: b.settings.language ?? null, promptStyle: b.settings.promptStyle ?? null, cleanup: Object.keys(b.settings.cleanup ?? {}).filter((k) => k !== "termPacks").length,
    styles: b.settings.styles ?? {}, styleNotes: Object.keys(b.settings.styleNotes ?? {}).length, appRules: rules.length, autoActions,
    termPacks: packs, outputLanguages: languages, reviewRewrites: review,
  };
  const hasSettings = !!detail && Object.values(detail).some((v) => (typeof v === "object" && v ? Object.keys(v).length : v));
  if (write) {
    dictionary = [...d.added.reverse(), ...dictionary];
    snippets = [...sn.added.reverse(), ...snippets];
    transforms = [...transforms, ...ac.added];
    notes = [...nt.added, ...notes];
    if (withSettings && hasSettings) {
      const { styles, styleNotes, cleanup, promptStyle, language } = b.settings;
      Object.assign(settings, { styles: { ...settings.styles, ...styles }, styleNotes: { ...settings.styleNotes, ...styleNotes },
        cleanup: { ...settings.cleanup, ...cleanup, termPacks: packs ? [...settings.cleanup.termPacks.filter((id) => !builtinIds.includes(id)), ...packs] : settings.cleanup.termPacks },
        outputLanguages: { ...settings.outputLanguages, ...languages }, ...(review && { reviewRewrites: review }), appCategories: { ...settings.appCategories, ...Object.fromEntries(rules) },
        autoTransforms: { ...settings.autoTransforms, ...Object.fromEntries(Object.entries(autoActions).map(([c, n]) => [c, transforms.find((t) => key(t.name) === key(n))!.id])) },
        ...(promptStyle && { promptStyle }), ...(language && { language }) });
      emit("settings-changed", structuredClone(settings));
    }
  }
  return { dictionary: d.counts, snippets: sn.counts, actions: ac.counts, notes: nt.counts.add,
    hasSettings: write ? withSettings && hasSettings : hasSettings, settings: (write ? withSettings && hasSettings : hasSettings) ? detail : null };
}

function parseDictionaryText(text: string) {
  const out: { phrase: string; replacement: string | null }[] = [];
  text.replace(/^\uFEFF/, "").split(/\r?\n/).forEach((raw, n) => {
    const line = raw.trimStart();
    if (!line.trim() || line.startsWith("#")) return;
    const sep = ["\t", " => ", " -> ", " \u2192 ", ","].find((s) => line.includes(s));
    const at = sep ? line.indexOf(sep) : -1;
    const phrase = (at < 0 ? line : line.slice(0, at)).trim();
    if (!phrase) throw `Line ${n + 1}: phrase is empty.`;
    out.push({ phrase, replacement: at < 0 ? null : line.slice(at + sep!.length).trim() || null });
  });
  return out;
}

/** Like the backend: a recovered row becomes "ok"; outside "all" its recording is then deleted. */
function retranscribe(id: number): Promise<string> {
  const item = history.find((x) => x.id === id);
  if (!item?.hasAudio) return Promise.reject(item ? "The recording is no longer available." : "This dictation was deleted.");
  const text = item.status === "ok" ? item.text : "Running five minutes late, start without me.";
  return new Promise((resolve) => setTimeout(() => {
    history = history.map((x) => (x.id === id ? { ...x, status: "ok", error: null, text, rawText: text.toLowerCase(), words: text.split(/\s+/).length,
      hasAudio: settings.privacy.audioRetention === "all" } : x));
    emit("history-updated");
    emit("dictation-outcome", { kind: "ok", message: "Recovered · copied to clipboard" });
    resolve(text);
  }, 700));
}

const aiKey = (id: string) => (id === "llm" ? `llm@${settings.llm.provider}:${(settings.llm.baseUrl ?? "").trim().replace(/\/+$/, "").toLowerCase()}` : id);

const handlers: Record<string, (a: any) => unknown> = {
  get_settings: () => structuredClone(settings),
  save_settings: ({ settings: s }) => {
    for (const k of Object.values(s.hotkeys) as string[]) if (!/\+?[A-Za-z0-9]+$/.test(k) || k.endsWith("+")) throw `Invalid hotkey "${k}"`;
    const turnedOnUpdates = s.checkUpdates && !settings.checkUpdates;
    // Like the backend: a provider key never follows its entry to another provider.
    for (const old of settings.sttChain) if (!s.sttChain.some((e: any) => e.id === old.id && e.provider === old.provider)) secrets.delete(`stt:${old.id}`);
    Object.assign(settings, s);
    if (turnedOnUpdates) setTimeout(() => emit("update-available", UPDATE), 3000);
    return structuredClone(settings);
  },
  reset_settings: () => handlers.save_settings({ settings: { ...settings, ...structuredClone(DEFAULT_PREFERENCES),
    llm: { ...settings.llm, enabled: false },
    cleanup: { removeFillers: true, voiceCommands: true, autoLearn: false, techTerms: true,
      termPacks: settings.cleanup.termPacks.filter((id) => !BUILTIN_PACKS.some((p) => p.id === id)) } } }),
  // Like the backend: the AI key ("llm") is kept per server.
  set_secret: ({ id }) => {
    if (id === "llm" && (settings.llm.provider === "cloud" || settings.llm.provider === "local")) throw "This AI service takes no key";
    secrets.add(aiKey(id));
  },
  delete_secret: ({ id }) => void secrets.delete(aiKey(id)),
  has_secrets: () => [...secrets].filter((id) => !id.startsWith("llm@")).concat(secrets.has(aiKey("llm")) ? ["llm"] : []),
  list_input_devices: () => ["Built-in Microphone", "USB Audio Device", "Headset (Bluetooth)"],
  list_history: ({ query }) => history.filter((x) => !query || x.text.toLowerCase().includes(query.toLowerCase())),
  update_history: ({ id, text }) => { history = history.map((x) => (x.id === id ? { ...x, text } : x)); return { added: [], pending: [], undo: [], full: false }; },
  undo_learned: () => {},
  repolish_history: ({ id }) => {
    const item = history.find((x) => x.id === id)!;
    const text = item.rawText.charAt(0).toUpperCase() + item.rawText.slice(1);
    history = history.map((x) => (x.id === id ? { ...x, text } : x));
    return new Promise((r) => setTimeout(() => r(text), 500));
  },
  retranscribe_history: ({ id }) => retranscribe(id),
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
  list_dictionary: () => dictionary.filter((x) => settings.cloud.teamLibrary || x.source !== "team"),
  upsert_dictionary: ({ entry }) => { own(dictionary, entry.id, (x) => x.source === "team"); let e; [dictionary, e] = upsert(dictionary, { ...entry, learned: false, source: "manual" }, { createdAt: iso(0) }); return e; },
  delete_dictionary: ({ id }) => void (own(dictionary, id, (x) => x.source === "team"), dictionary = dictionary.filter((x) => x.id !== id)),
  list_snippets: () => snippets.filter((x) => settings.cloud.teamLibrary || !x.team),
  upsert_snippet: ({ snippet }) => { own(snippets, snippet.id, (x) => x.team); let e; [snippets, e] = upsert(snippets, snippet, { createdAt: iso(0) }); return e; },
  delete_snippet: ({ id }) => void (own(snippets, id, (x) => x.team), snippets = snippets.filter((x) => x.id !== id)),
  list_transforms: () => transforms.filter((x) => settings.cloud.teamLibrary || !x.team)
    .map((x) => ({ ...x, edited: edited(x), defaultName: x.builtin ? builtinDefaults[x.id]?.name : undefined })),
  upsert_transform: ({ transform }) => { own(transforms, transform.id, (x) => x.team); let e; [transforms, e] = upsert(transforms, transform, { builtin: false, output: "replace" }); return e; },
  delete_transform: ({ id }) => void (own(transforms, id, (x) => x.team), transforms = transforms.filter((x) => x.id !== id || x.builtin)),
  restore_transform: ({ id }) => {
    own(transforms, id, (x) => x.team);
    const t = transforms.find((x) => x.id === id);
    if (!t?.builtin) throw "Only built-in actions can be restored to their default";
    if (!builtinDefaults[id]) throw "This built-in action's default isn't known, so it can't be restored";
    const restored = { ...t, ...builtinDefaults[id] };
    transforms = transforms.map((x) => (x.id === id ? restored : x));
    return restored;
  },
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
  polish_model_status: () => ({ ...(polishModels.find((m) => m.id === settings.llm.model) ?? polishModels[0]) }),
  polish_models: () => polishModels.map((m) => ({ ...m })),
  download_polish_model: ({ id }) => {
    const m = polishModels.find((x) => x.id === (id ?? "s1-mini"))!;
    const total = m.sizeMb * 1e6;
    let downloaded = 0;
    m.downloading = true;
    const t = setInterval(() => {
      downloaded = Math.min(total, downloaded + total / 12);
      const done = downloaded >= total;
      if (done) { clearInterval(t); m.installed = true; m.downloading = false; }
      emit("model-download", { id: m.id, downloaded, total, done, error: null });
    }, 250);
  },
  delete_polish_model: ({ id }) => void (polishModels.find((x) => x.id === (id ?? "s1-mini"))!.installed = false),
  delete_local_model: ({ id }) => void (models.find((x) => x.id === id)!.installed = false),
  test_llm: () => new Promise((r) => setTimeout(() => r({ text: "So let's meet at 3pm on Friday", latencyMs: 640 }), 600)),
  test_stt: () => new Promise((r) => setTimeout(() => r({ text: "The quick brown fox jumps over the lazy dog.", latencyMs: 412 }), 700)),
  start_dictation: () => emit("dictation-state", { status: "recording", mode: "dictate", handsFree: true, stage: "", micReady: true, workspace: "opstrellis" }),
  stop_dictation: () => {
    emit("dictation-state", { status: "processing", mode: "dictate", handsFree: false, stage: "transcribing", micReady: true });
    setTimeout(() => {
      emit("dictation-result", { text: "Hello from the OpsTrellis dev mock.", latencyMs: 380 });
      emit("dictation-outcome", { kind: "ok", message: "Inserted · 0.4 s" });
      emit("dictation-state", { status: "idle", mode: "dictate", handsFree: false, stage: "", micReady: false });
    }, 800);
  },
  question_shown: () => {},
  cancel_dictation: () => emit("dictation-state", { status: "idle", mode: "dictate", handsFree: false, stage: "", micReady: false }),
  paste_last: () => undefined,
  cloud_login: () => {
    setTimeout(() => emit("cloud-status", { signedIn: true, email: "alex@example.com", plan: "free", usage: { minutesUsed: 4.2, minutesLimit: 15 } }), 4000);
    return { userCode: "WXYZ-1234", verificationUrl: "https://cloud.opstrellis.app/device?code=WXYZ-1234" };
  },
  cloud_logout: () => ({ signedIn: false, email: null, plan: null, usage: null }),
  cloud_status: () => ({ signedIn: false, email: null, plan: null, usage: null }),
  team_status: () => teamStatus(),
  team_sync_now: () => new Promise((r) => setTimeout(() => { teamSyncedAt = iso(0); r(teamStatus()); }, 500)),
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
  export_backup: ({ includeNotes }) => ({
    path: "~/Downloads/opstrellis-backup-20260101-120000.json",
    dictionary: dictionary.filter((x) => x.source !== "team").length, snippets: snippets.filter((x) => !x.team).length, actions: transforms.filter((t) => !t.builtin && !t.team).length,
    notes: includeNotes ? notes.length : 0, skipped: 0,
  }),
  preview_backup: ({ json }) => mergeBackup(json, false, true),
  import_backup: ({ json, settings: withSettings, notes: withNotes }) => mergeBackup(json, true, withNotes, withSettings),
  import_dictionary_text: ({ text }) => {
    const seen = new Set(dictionary.map((x) => key(x.phrase)));
    const added: DictEntry[] = [];
    let skip = 0;
    for (const e of parseDictionaryText(text)) {
      if (seen.has(key(e.phrase))) skip++;
      else { seen.add(key(e.phrase)); added.push({ id: nextId++, ...e, createdAt: iso(0), learned: false }); }
    }
    dictionary = [...added.reverse(), ...dictionary];
    return { add: added.length, skip };
  },
  list_term_packs: () => termPacks().map((p) => ({ ...p, enabled: p.id === "tech" ? settings.cleanup.techTerms : settings.cleanup.termPacks.includes(p.id) })),
  preview_term_pack: ({ json }) => {
    const { pack, entries } = parseTermPack(json);
    const replacements = entries.flatMap((e) => [
      ...(e.aliases ?? []).map((alias) => ({ alias, term: e.term, codeOnly: false })),
      ...(e.techAliases ?? []).map((alias) => ({ alias, term: e.term, codeOnly: true })),
    ]);
    return { ...pack, dropped: 0, replaces: userPacks.some((x) => x.id === pack.id), replacements: replacements.slice(0, 100), replacementCount: replacements.length };
  },
  import_term_pack: ({ json }) => {
    const { pack } = parseTermPack(json);
    userPacks = [...userPacks.filter((x) => x.id !== pack.id), pack].sort((a, b) => a.id.localeCompare(b.id));
    return { pack: { ...pack, enabled: settings.cleanup.termPacks.includes(pack.id) }, dropped: 0 };
  },
  list_workspaces: () => structuredClone(workspaces),
  add_workspace: ({ path }) => addWorkspace(path),
  remove_workspace: ({ path }) => { workspaces = workspaces.filter((w) => w.path !== path); return structuredClone(workspaces); },
  reindex_workspace: ({ path }) => {
    if (!workspaces.some((w) => w.path === path)) throw "That folder isn't a workspace.";
    return indexWorkspace(path);
  },
  delete_term_pack: ({ id }) => {
    if (!userPacks.some((p) => p.id === id)) throw "Only imported packs can be deleted.";
    userPacks = userPacks.filter((p) => p.id !== id);
    settings.cleanup.termPacks = settings.cleanup.termPacks.filter((x) => x !== id);
  },
  check_update: () => new Promise((r) => setTimeout(() => r(UPDATE), 600)),
  self_update_supported: () => !location.search.includes("noSelfUpdate"),
  download_update: () => downloadUpdate(),
  install_update: () => {
    if (updateStage !== "ready") throw "Download the update first.";
    emit("update-download", { state: "installing", percent: 100, error: null });
    if (location.search.includes("installError")) {
      return new Promise((_, reject) => setTimeout(() => {
        emit("update-download", { state: "ready", percent: 100, error: null });
        reject("Couldn't install the update: Read-only file system (os error 30)");
      }, 800));
    }
    // Like the restart: the page starts over on the "new version".
    setTimeout(() => location.reload(), 800);
    return new Promise(() => {});
  },
  open_release_page: ({ tag }) => {
    if (!/^v?\d+\.\d+\.\d+$/.test(tag)) throw "Not a release version";
    window.open(`https://github.com/mtr1920/opstrellis/releases/tag/${tag}`, "_blank", "noopener");
  },
};

export async function mockInvoke(cmd: string, args: Record<string, unknown> = {}): Promise<unknown> {
  const fn = handlers[cmd];
  if (!fn) throw `mock: unknown command ${cmd}`;
  return fn(args);
}

const REWRITE = "Dear Ms. Patel,\n\nThank you for sending the revised proposal. I have reviewed the pricing and the delivery schedule with our team, and we are happy to proceed on the terms you outlined.\n\nCould you please confirm the start date and send the contract for signature? We would also appreciate a short call next week to agree on the first milestones.\n\nKind regards,\nAlex";

/** Command-mode review, as the backend runs it: the card waits for Enter / Esc (here: keys in this page). */
function mockReview() {
  emit("dictation-state", { status: "review", mode: "command", handsFree: false, stage: "", micReady: true, text: REWRITE });
  const end = (insert: boolean) => {
    window.removeEventListener("keydown", onKey);
    emit("dictation-outcome", insert ? { kind: "ok", message: "Inserted · 2.1 s" } : { kind: "warn", message: "Discarded. Paste last inserts it anyway." });
    emit("dictation-state", { status: "idle", mode: "command", handsFree: false, stage: "", micReady: false });
  };
  const onKey = (e: KeyboardEvent) => { if (e.key === "Enter" || e.key === "Escape") end(e.key === "Enter"); };
  window.addEventListener("keydown", onKey);
}

const MOCK_QUESTION = "The migration renames the column user_name to username in three tables. Should I deploy the hotfix to staging now and run the backfill tonight, or wait for the nightly build and do both tomorrow morning? Also: do you want me to notify the data team before the change goes out?";

/** An agent's question (with an ordinary dictation's outcome meanwhile), then the user answering it
 * by voice, on repeat. `?question=<text>#overlay` shows that text instead. */
function mockQuestion() {
  const idle = { status: "idle", mode: "dictate", handsFree: false, stage: "", micReady: false } as const;
  const question = new URLSearchParams(location.search).get("question") || MOCK_QUESTION;
  const cycle = () => {
    emit("agent-question", question);
    setTimeout(() => emit("dictation-outcome", { kind: "ok", message: "Inserted · 0.6 s" }), 2500);
    setTimeout(() => emit("dictation-state", { status: "recording", mode: "dictate", handsFree: false, stage: "", micReady: true, answering: true }), 7000);
    setTimeout(() => emit("dictation-state", { status: "processing", mode: "dictate", handsFree: false, stage: "transcribing", micReady: true }), 10000);
    setTimeout(() => {
      emit("agent-question", null);
      emit("dictation-outcome", { kind: "ok", message: "Sent to the agent" });
      emit("dictation-state", idle);
    }, 11000);
  };
  cycle();
  setInterval(cycle, 13000);
  let i = 0;
  setInterval(() => emit("audio-level", 0.15 + Math.abs(Math.sin(i++ / 3)) * 0.7 * Math.random()), 50);
}

// Overlay demo: animate the pill so it can be previewed in a browser (`?review#overlay`: the review
// card; `?question#overlay`: an agent's question and the answer being dictated).
if (location.hash === "#overlay" && location.search.includes("review")) {
  setTimeout(mockReview, 50);
} else if (location.hash === "#overlay" && location.search.includes("question")) {
  setTimeout(mockQuestion, 50);
} else if (location.hash === "#overlay") {
  const words = "so I was thinking we could move the standup to ten tomorrow".split(" ");
  let i = 0;
  setTimeout(() => emit("dictation-state", { status: "recording", mode: "dictate", handsFree: true, stage: "", micReady: true, workspace: "opstrellis" }), 50);
  setInterval(() => emit("audio-level", 0.15 + Math.abs(Math.sin(i++ / 3)) * 0.7 * Math.random()), 50);
  setInterval(() => emit("partial-transcript", words.slice(0, (i / 6) % words.length + 1).join(" ")), 300);
}
