// Typed wrappers around the Tauri IPC contract (docs/DESKTOP_IPC.md).
// In a plain browser (`npm run dev` without Tauri) calls go to the in-memory mock.
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef } from "react";
import { mockInvoke, mockListen } from "./mock";

export type Tone = "formal" | "casual" | "very_casual" | "excited";
export type Category = "personal" | "work" | "email" | "ai" | "other";
export type ProviderKind = "local" | "openai" | "deepgram" | "assemblyai" | "google" | "azure" | "elevenlabs" | "cloud";

export interface SttEntry {
  id: string;
  provider: ProviderKind;
  model: string;
  baseUrl?: string | null;
  region?: string | null;
  streaming: boolean;
  enabled: boolean;
}

export interface Settings {
  hotkeys: { dictate: string; command: string; pasteLast: string };
  microphone: string | null;
  language: string;
  sttChain: SttEntry[];
  /** "local" = S1-mini by Superwhisper on this device (polish only). */
  llm: { enabled: boolean; provider: "openai" | "anthropic" | "cloud" | "local"; baseUrl: string | null; model: string };
  cleanup: { removeFillers: boolean; voiceCommands: boolean; autoLearn: boolean; techTerms: boolean };
  styles: Record<Category, Tone>;
  autoTransforms: Partial<Record<Category, number>>; // category -> transform id applied after every dictation
  /** The user's app rules: lowercase app name or window-title text -> insight category. */
  appCategories: Record<string, string>;
  /** How the user writes in each kind of app, for AI cleanup. */
  styleNotes: Partial<Record<Category, string>>;
  promptStyle: "clean" | "structured";
  insertion: { method: "paste" | "type"; restoreClipboard: boolean; pasteDelayMs: number };
  privacy: { historyRetentionDays: number | null; syncUsage: boolean };
  cloud: { baseUrl: string; email: string | null };
  overlay: boolean;
  sounds: boolean;
  launchAtLogin: boolean;
  showInDock: boolean;
  theme: Theme;
  onboarded: boolean;
}

export interface HistoryItem {
  id: number; createdAt: string; rawText: string; text: string;
  appName: string | null; category: string; provider: string | null; model: string | null;
  audioMs: number; latencyMs: number; words: number;
  status: "ok" | "cancelled" | "error"; error: string | null; mode: "dictate" | "command";
}

export interface AppUse { appName: string; category: string; count: number }

export interface Stats {
  totalWords: number; totalDictations: number; wpm: number; fixes: number;
  streak: number; longestStreak: number;
  days: { date: string; words: number }[];
  categories: { category: string; count: number; words: number }[];
}

/** What a correction taught: entries added, and fixes that are learned if made once more. */
export interface Learned { added: DictEntry[]; pending: { phrase: string; replacement: string | null }[]; undo: UndoRow[] }
/** A dictionary row learning changed: [phrase, replacement] before and after (null = no row). */
export interface UndoRow { id: number; before: [string, string | null] | null; after: [string, string | null] | null }
export interface DictEntry { id: number; phrase: string; replacement: string | null; createdAt: string; learned: boolean }
export interface Snippet { id: number; trigger: string; expansion: string; createdAt: string }
/** output: "replace" the selection (or insert at the cursor), or "copy" the result to the clipboard. */
export interface Transform { id: number; name: string; prompt: string; builtin: boolean; output: "replace" | "copy" }
export interface Note { id: number; title: string; body: string; updatedAt: string; kind: "note" | "meeting" }
export interface LocalModel {
  id: string; name: string; languages: string; sizeMb: number; installed: boolean; downloading: boolean; description: string;
  langs: string[]; recommended: boolean;
}
export interface CloudStatus { signedIn: boolean; email: string | null; plan: string | null; usage: any | null }
export interface DictationState {
  status: "idle" | "recording" | "processing"; mode: "dictate" | "command"; handsFree: boolean;
  /** While processing. */
  stage: "" | "transcribing" | "polishing";
  /** While recording: the microphone delivers audio. */
  micReady: boolean;
}
/** How a dictation ended, shown briefly in the pill. */
export interface Outcome { kind: "ok" | "warn" | "error"; message: string }
export interface Permissions {
  accessibility: boolean; platform: "macos" | "linux"; wayland: boolean; hotkeyError: string | null;
  /** null unless a single key (Fn, Right ⌥, …) is a shortcut (macOS). */
  inputMonitoring: boolean | null;
  /** What macOS itself does on an Fn press ("shows Emoji & Symbols"), when Fn is a shortcut. */
  fnSystemAction: string | null;
  /** null unless Wayland. */
  waylandShortcuts: WaylandShortcuts | null;
  /** null unless Wayland: whether a tool can send the paste keys. */
  insertion: InsertionStatus | null;
}
/** The desktop's GlobalShortcuts portal session (Wayland). `trigger` is empty when no key is assigned. */
export type PortalState =
  | { state: "unavailable" } | { state: "binding" } | { state: "failed"; error: string }
  | { state: "bound"; shortcuts: { id: "dictate" | "command" | "paste-last"; trigger: string }[] };
export interface WaylandShortcuts {
  portal: PortalState;
  gnome: boolean;
  /** OpsTrellis's GNOME custom shortcuts (running `opstrellis --toggle`, …) are set up. */
  gnomeShortcuts: boolean;
}
export interface InsertionStatus { ready: "wtype" | "ydotool" | null; problem: string | null; fix: string | null }
export interface PolishModel { name: string; sizeMb: number; installed: boolean; downloading: boolean }
export interface ModelDownload { id: string; downloaded: number; total: number; done: boolean; error: string | null; cancelled?: boolean }

export type Theme = "system" | "light" | "dark";

export interface Events {
  "theme-changed": Theme;
  "dictation-state": DictationState;
  "audio-level": number;
  "partial-transcript": string;
  "dictation-result": { text: string; latencyMs: number };
  "dictation-error": string;
  "dictation-outcome": Outcome;
  /** A single key (Fn, RightOption, …) pressed on its own while the shortcut recorder is open (macOS). */
  "shortcut-captured": string;
  "history-updated": null;
  "model-download": ModelDownload;
  "cloud-status": CloudStatus;
  "meeting-transcript": string;
  "meeting-done": { noteId: number | null; error: string | null };
  /** A meeting recording started or stopped (also from the tray). */
  "meeting-status": null;
  /** Settings changed outside this window (tray). */
  "settings-changed": Settings;
  /** Something `permissions()` reports changed (e.g. the desktop bound the Wayland shortcuts). */
  "permissions-changed": null;
}

export interface MeetingStatus { active: boolean; finishing: boolean; startedAt: string | null; transcript: string }

export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return (isTauri ? invoke<T>(cmd, args) : mockInvoke(cmd, args)) as Promise<T>;
}

export const api = {
  getSettings: () => call<Settings>("get_settings"),
  saveSettings: (settings: Settings) => call<Settings>("save_settings", { settings }),
  setSecret: (id: string, value: string) => call<void>("set_secret", { id, value }),
  deleteSecret: (id: string) => call<void>("delete_secret", { id }),
  hasSecrets: () => call<string[]>("has_secrets"),
  listInputDevices: () => call<string[]>("list_input_devices"),

  listHistory: (query: string, limit = 200, offset = 0) =>
    call<HistoryItem[]>("list_history", { query: query || undefined, limit, offset }),
  deleteHistory: (id: number) => call<void>("delete_history", { id }),
  /** Saves a corrected dictation; returns what auto-learn took from the correction. */
  updateHistory: (id: number, text: string) => call<Learned>("update_history", { id, text }),
  /** Runs cleanup again on the original transcript; returns the new text (nothing is learned). */
  repolishHistory: (id: number) => call<string>("repolish_history", { id }),
  /** Reverts what a correction taught; entries changed since are left alone. */
  undoLearned: (rows: UndoRow[]) => call<void>("undo_learned", { rows }),
  clearHistory: () => call<void>("clear_history"),
  listApps: () => call<AppUse[]>("list_apps"),
  getStats: () => call<Stats>("get_stats"),

  listDictionary: () => call<DictEntry[]>("list_dictionary"),
  upsertDictionary: (entry: { id?: number; phrase: string; replacement: string | null }) =>
    call<DictEntry>("upsert_dictionary", { entry }),
  deleteDictionary: (id: number) => call<void>("delete_dictionary", { id }),

  listSnippets: () => call<Snippet[]>("list_snippets"),
  upsertSnippet: (snippet: { id?: number; trigger: string; expansion: string }) =>
    call<Snippet>("upsert_snippet", { snippet }),
  deleteSnippet: (id: number) => call<void>("delete_snippet", { id }),

  listTransforms: () => call<Transform[]>("list_transforms"),
  upsertTransform: (transform: { id?: number; name: string; prompt: string; output: Transform["output"] }) =>
    call<Transform>("upsert_transform", { transform }),
  deleteTransform: (id: number) => call<void>("delete_transform", { id }),

  listNotes: (kind?: Note["kind"]) => call<Note[]>("list_notes", { kind }),
  upsertNote: (note: { id?: number; title: string; body: string }) => call<Note>("upsert_note", { note }),
  deleteNote: (id: number) => call<void>("delete_note", { id }),

  listLocalModels: () => call<LocalModel[]>("list_local_models"),
  downloadLocalModel: (id: string) => call<void>("download_local_model", { id }),
  cancelLocalDownload: (id: string) => call<void>("cancel_local_download", { id }),
  deleteLocalModel: (id: string) => call<void>("delete_local_model", { id }),
  polishModelStatus: () => call<PolishModel>("polish_model_status"),
  downloadPolishModel: () => call<void>("download_polish_model"),
  deletePolishModel: () => call<void>("delete_polish_model"),
  testStt: (entryId: string) => call<{ text: string; latencyMs: number }>("test_stt", { entryId }),
  testLlm: () => call<{ text: string; latencyMs: number }>("test_llm"),

  startDictation: () => call<void>("start_dictation"),
  stopDictation: () => call<void>("stop_dictation"),
  cancelDictation: () => call<void>("cancel_dictation"),
  pasteLast: () => call<void>("paste_last"),

  cloudLogin: () => call<{ userCode: string; verificationUrl: string }>("cloud_login"),
  cloudLogout: () => call<CloudStatus>("cloud_logout"),
  cloudStatus: () => call<CloudStatus>("cloud_status"),

  meetingStart: (secondDevice: string | null, template: string) => call<void>("meeting_start", { secondDevice, template }),
  meetingTemplates: () => call<{ id: string; name: string }[]>("meeting_templates"),
  /** Drafts a follow-up from a saved meeting: "email" or "crm". */
  meetingFollowUp: (noteId: number, kind: "email" | "crm") => call<string>("meeting_follow_up", { noteId, kind }),
  meetingStop: () => call<void>("meeting_stop"),
  meetingStatus: () => call<MeetingStatus>("meeting_status"),

  permissions: () => call<Permissions>("permissions"),
  requestAccessibility: () => call<void>("request_accessibility"),
  requestInputMonitoring: () => call<void>("request_input_monitoring"),
  setShortcutRecording: (on: boolean) => call<void>("set_shortcut_recording", { on }),
  /** Wayland: ask the desktop to bind the shortcuts again (GlobalShortcuts portal). */
  rebindWaylandShortcuts: () => call<void>("rebind_wayland_shortcuts"),
  /** GNOME without the portal: add custom shortcuts that run `opstrellis --toggle` and friends. */
  addGnomeShortcuts: () => call<void>("add_gnome_shortcuts"),
  openLogs: () => call<void>("open_logs"),
  version: () => (isTauri ? getVersion() : Promise.resolve("dev")),
};

/** Subscribe to a backend event; returns an unsubscribe function. */
export function on<K extends keyof Events>(event: K, cb: (payload: Events[K]) => void): () => void {
  if (!isTauri) return mockListen(event, cb as (p: unknown) => void);
  const p = listen<Events[K]>(event, (e) => cb(e.payload));
  return () => void p.then((unlisten) => unlisten());
}

/** React hook: subscribe for the component's lifetime, always calling the latest callback. */
export function useEvent<K extends keyof Events>(event: K, cb: (payload: Events[K]) => void) {
  const ref = useRef(cb);
  ref.current = cb;
  useEffect(() => on(event, (p) => ref.current(p)), [event]);
}

export const errMsg = (e: unknown) => (typeof e === "string" ? e : e instanceof Error ? e.message : String(e));
