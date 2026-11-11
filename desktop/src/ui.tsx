// Small shared UI pieces and helpers.
import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEvent, type ModelDownload, type Settings, type Theme } from "./api";
import { Icon } from "./icons";

export type SetSetting = <K extends keyof Settings>(key: K, value: Settings[K]) => void;
export interface SettingsProps { settings: Settings; set: SetSetting }

export function Toggle({ checked, onChange, label }: { checked: boolean; onChange: (v: boolean) => void; label?: string }) {
  return (
    <button type="button" role="switch" aria-checked={checked} aria-label={label}
      className={"toggle" + (checked ? " on" : "")} onClick={() => onChange(!checked)}>
      <span />
    </button>
  );
}

/** A labelled settings row: text on the left, control on the right. */
export function Row({ title, desc, children }: { title: ReactNode; desc?: ReactNode; children: ReactNode }) {
  return (
    <div className="row">
      <div className="row-text">
        <div className="row-title">{title}</div>
        {desc && <div className="row-desc">{desc}</div>}
      </div>
      <div className="row-control">{children}</div>
    </div>
  );
}

export function Search({ value, onChange, placeholder = "Search" }: { value: string; onChange: (v: string) => void; placeholder?: string }) {
  return (
    <label className="search">
      <Icon name="search" />
      <input value={value} onChange={(e) => onChange(e.target.value)} placeholder={placeholder} aria-label={placeholder} />
    </label>
  );
}

/** The webview runs on macOS (the browser preview follows the machine it runs on). */
export const IS_MAC = /Mac/.test(navigator.userAgent);

const darkQuery = matchMedia("(prefers-color-scheme: dark)");
let chosenTheme: Theme = "system";
const paintTheme = () => {
  const root = document.documentElement;
  const theme = chosenTheme === "dark" || (chosenTheme === "system" && darkQuery.matches) ? "dark" : "light";
  if (root.dataset.theme === theme) return;
  root.classList.add("theme-switching");
  root.dataset.theme = theme;
  requestAnimationFrame(() => requestAnimationFrame(() => root.classList.remove("theme-switching")));
};
darkQuery.addEventListener("change", paintTheme);

/** Applies the appearance choice; remembered so the next launch paints the right theme at once. */
export function applyTheme(theme: Theme | null | undefined) {
  const next = theme === "light" || theme === "dark" ? theme : "system";
  const changed = next !== chosenTheme;
  chosenTheme = next;
  try { localStorage.setItem("theme", chosenTheme); } catch {}
  paintTheme();
  if (changed && "__TAURI_INTERNALS__" in window) getCurrentWindow().setTheme(next === "system" ? null : next).catch(() => {});
}

export const isDarkTheme = (t: Theme) => t === "dark" || (t !== "light" && darkQuery.matches);

export function initialTheme(): Theme {
  try { return (localStorage.getItem("theme") as Theme | null) ?? "system"; } catch { return "system"; }
}

// Shortcuts are stored as accelerators ("Ctrl+Shift+Space", "Super+K", "Fn", "RightOption") and shown with each
// platform's own key names: ⌃ Control, ⌥ Option, ⇧ Shift, ⌘ Command on macOS; Ctrl, Alt, Shift, Super on Linux.
const MAC_KEYS: Record<string, [symbol: string, name: string]> = {
  Ctrl: ["⌃", "Control"], Control: ["⌃", "Control"], Alt: ["⌥", "Option"], Option: ["⌥", "Option"],
  Shift: ["⇧", "Shift"], Super: ["⌘", "Command"], Cmd: ["⌘", "Command"], Command: ["⌘", "Command"], Meta: ["⌘", "Command"],
  Fn: ["🌐", "fn"], RightCommand: ["⌘", "Right Command"], RightOption: ["⌥", "Right Option"],
  RightControl: ["⌃", "Right Control"], RightShift: ["⇧", "Right Shift"], Up: ["↑", "Up"], Down: ["↓", "Down"], Left: ["←", "Left"], Right: ["→", "Right"],
  Enter: ["↩", "Return"], Backspace: ["⌫", "Delete"], Delete: ["⌦", "Forward Delete"], Tab: ["⇥", "Tab"],
};
const LINUX_KEYS: Record<string, string> = {
  Control: "Ctrl", Option: "Alt", Cmd: "Super", Command: "Super", Meta: "Super",
};
/** Punctuation keys are recorded by their key code names ("BracketLeft"); show the character. */
const PUNCTUATION: Record<string, string> = {
  Minus: "-", Equal: "=", BracketLeft: "[", BracketRight: "]", Backslash: "\\", Semicolon: ";",
  Quote: "'", Comma: ",", Period: ".", Slash: "/", Backquote: "`",
};

/** One key's label: [symbol or "", name]. */
export function keyLabel(key: string, mac = IS_MAC): [string, string] {
  if (PUNCTUATION[key]) return ["", PUNCTUATION[key]];
  if (mac) return MAC_KEYS[key] ?? ["", key];
  return ["", LINUX_KEYS[key] ?? key];
}

/** Keys that are a shortcut on their own (macOS); dictate and command mode can use one of them. */
export const SINGLE_KEYS = ["Fn", "RightCommand", "RightOption", "RightControl", "RightShift"];
export const isSingleKey = (combo: string) => SINGLE_KEYS.includes(combo);

/** A shortcut as compact text: "⌃⇧Space" or "Right ⌥" on macOS, "Ctrl+Shift+Space" on Linux. */
export function comboText(combo: string, mac = IS_MAC): string {
  const keys = combo.split("+").filter(Boolean).map((k) => keyLabel(k, mac));
  if (!mac) return keys.map(([, name]) => name).join("+");
  return keys.map(([sym, name]) => (sym && name.startsWith("Right ") ? "Right " + sym : sym || name)).join("");
}

/** A shortcut rendered as keycaps with the platform's key names. */
export function Keys({ combo }: { combo: string }) {
  return (
    <span className="keys" aria-label={comboText(combo)}>
      {combo.split("+").filter(Boolean).map((k, i) => {
        const [sym, name] = keyLabel(k);
        return <kbd key={i} title={name}>{sym && <span className="kbd-sym" aria-hidden="true">{sym}</span>}{name}</kbd>;
      })}
    </span>
  );
}

// Rendered into <body> so the fixed scrim covers the whole window, whatever the ancestors' stacking contexts.
export function Modal({ title, onClose, children, footer, className }: {
  title: ReactNode; onClose: () => void; children: ReactNode; footer?: ReactNode; className?: string;
}) {
  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape" && !e.defaultPrevented && !e.isComposing) close.current(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  return createPortal(
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className={"modal" + (className ? " " + className : "")} role="dialog" aria-label={typeof title === "string" ? title : undefined}>
        <div className="modal-head">
          {typeof title === "string" ? <h3>{title}</h3> : title}
          <button className="icon-btn" onClick={onClose} aria-label="Close"><Icon name="x" /></button>
        </div>
        <div className="modal-body">{children}</div>
        {footer && <div className="modal-foot">{footer}</div>}
      </div>
    </div>,
    document.body,
  );
}

/** A group of pages shown as tabs under one nav item (e.g. Library: Dictionary, Snippets, Actions). */
export const PageGroup = createContext<{ title: string; tabs: ReactNode } | null>(null);

/** Tabs for a page group, rendered in the page header next to the group's title. */
export function Tabs<T extends string>({ tabs, value, onChange, label }: { tabs: [T, string][]; value: T; onChange: (v: T) => void; label: string }) {
  return (
    <div className="segmented page-tabs" role="tablist" aria-label={label}>
      {tabs.map(([id, name]) => (
        <button key={id} role="tab" aria-selected={value === id} className={value === id ? "active" : ""} onClick={() => onChange(id)}>{name}</button>
      ))}
    </div>
  );
}

/** A page: one translucent toolbar row (title, tabs, actions) over the content. Inside a
 * page group, the group's title and tabs replace the page's own title. */
export function Page({ title, actions, children, className }: { title: string; actions?: ReactNode; children?: ReactNode; className?: string }) {
  const group = useContext(PageGroup);
  return (
    <div className={"page" + (className ? " " + className : "")}>
      <header className="page-head" data-tauri-drag-region>
        <div className="page-head-inner" data-tauri-drag-region>
          <h1 className="page-title" data-tauri-drag-region>{group?.title ?? title}</h1>
          {group?.tabs}
          {actions && <div className="page-actions">{actions}</div>}
        </div>
      </header>
      <div className="page-body">{children}</div>
    </div>
  );
}

/** A titled group: heading row (with optional right-side content), optional description, then content. */
export function Section({ title, aside, desc, children, id }: { title: string; aside?: ReactNode; desc?: ReactNode; children: ReactNode; id?: string }) {
  return (
    <section className="section" id={id}>
      <div className="section-head">
        <h2 className="section-title">{title}</h2>
        {aside && <div className="section-aside">{aside}</div>}
      </div>
      {desc && <p className="section-desc">{desc}</p>}
      {children}
    </section>
  );
}

/** A one-line empty state: short sentence plus at most one button. */
export function Empty({ children, action }: { children: ReactNode; action?: ReactNode }) {
  return (
    <div className="card empty">
      <span>{children}</span>
      {action}
    </div>
  );
}

/** Tracks `model-download` progress events, keyed by model id. `onDone` runs when a download
 * installs or is cancelled. */
export function useModelDownloads(onDone?: (id: string, cancelled: boolean) => void) {
  const [progress, setProgress] = useState<Record<string, ModelDownload>>({});
  useEvent("model-download", (p) => {
    setProgress((prev) => {
      const next = { ...prev, [p.id]: p };
      if (p.done && !p.error) delete next[p.id];
      return next;
    });
    if (p.done && !p.error) onDone?.(p.id, !!p.cancelled);
  });
  return progress;
}

export function Progress({ value }: { value: number }) {
  return <div className="progress"><div style={{ width: `${Math.round(value * 100)}%` }} /></div>;
}

export const CATEGORY_LABELS: Record<string, string> = {
  ai_prompts: "AI prompts", work_messages: "Work messages", emails: "Emails",
  documents: "Documents", personal_messages: "Personal messages", other: "Other",
};

export const fmtNum = (n: number) => n.toLocaleString();
export const fmtTime = (iso: string) => new Date(iso).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
export const localDate = (d: Date) =>
  `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;

export function dayLabel(iso: string) {
  const d = new Date(iso);
  const today = new Date();
  const yesterday = new Date(Date.now() - 864e5);
  if (localDate(d) === localDate(today)) return "Today";
  if (localDate(d) === localDate(yesterday)) return "Yesterday";
  return d.toLocaleDateString([], { weekday: "long", month: "long", day: "numeric" });
}

export function useSearch<T>(items: T[], text: (t: T) => string) {
  const [query, setQuery] = useState("");
  const q = query.trim().toLowerCase();
  const filtered = q ? items.filter((i) => text(i).toLowerCase().includes(q)) : items;
  return { query, setQuery, filtered };
}

const LOCAL_SUFFIXES = [".local", ".lan", ".internal", ".home", ".home.arpa", ".localdomain", ".ts.net"];

function localV4(a: number, b: number) {
  return a === 127 || a === 10 || (a === 172 && b >= 16 && b < 32) || (a === 192 && b === 168) || (a === 100 && b >= 64 && b < 128) || (a === 169 && b === 254);
}

/**
 * What an `http://` address risks, mirroring the backend (`endpoint.rs`): "refused" for a host beyond
 * the local network (nothing is sent there), "unencrypted" for one on it other than this computer.
 */
export function httpRisk(url: string | null | undefined): "refused" | "unencrypted" | null {
  let u: URL;
  try { u = new URL((url ?? "").trim()); } catch { return null; }
  if (u.protocol !== "http:") return null;
  const host = u.hostname.replace(/\.$/, "");
  if (["localhost", "[::1]", "[::]", "0.0.0.0"].includes(host) || /^127\.\d+\.\d+\.\d+$/.test(host)) return null;
  const v4 = host.match(/^(\d+)\.(\d+)\.\d+\.\d+$/);
  const v6 = host.match(/^\[(.*)\]$/)?.[1];
  const mapped = v6?.match(/^::ffff:([0-9a-f]{1,4}):[0-9a-f]{1,4}$/);
  const first = v6 ? parseInt(v6.split(":")[0] || "0", 16) : 0;
  const local = v4 ? localV4(+v4[1], +v4[2])
    : mapped ? localV4(parseInt(mapped[1], 16) >> 8, parseInt(mapped[1], 16) & 255)
    : v6 !== undefined ? (first & 0xfe00) === 0xfc00 || (first & 0xffc0) === 0xfe80
    : !host.includes(".") || LOCAL_SUFFIXES.some((s) => host.endsWith(s) && host.length > s.length);
  return local ? "unencrypted" : "refused";
}

export function UnencryptedWarning({ url }: { url: string | null | undefined }) {
  const risk = httpRisk(url);
  if (risk === "refused") return <div className="error">OpsTrellis won't send anything to this address. Use https://.</div>;
  return risk ? <div className="error">Not encrypted: anyone on this network can read your audio, text and key.</div> : null;
}
