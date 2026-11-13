import { useEffect, useRef, useState } from "react";
import { api, errMsg, useEvent, type DictationState, type Permissions, type Settings, type UpdateDownload, type UpdateInfo } from "./api";
import { Icon, Logo, type IconName } from "./icons";
import { applyTheme, comboText, IS_MAC, isDarkTheme, PageGroup, Tabs, type SetSetting } from "./ui";
import Home from "./pages/Home";
import Insights from "./pages/Insights";
import Dictionary from "./pages/Dictionary";
import Snippets from "./pages/Snippets";
import Style from "./pages/Style";
import Transforms from "./pages/Transforms";
import Scratchpad from "./pages/Scratchpad";
import Notetaker from "./pages/Notetaker";
import SettingsPage from "./pages/Settings";
import Onboarding from "./pages/Onboarding";

const PAGES: { id: string; label: string; icon: IconName }[] = [
  { id: "home", label: "Home", icon: "home" },
  { id: "notes", label: "Notes", icon: "note" },
  { id: "library", label: "Library", icon: "book" },
  { id: "style", label: "Style", icon: "style" },
  { id: "insights", label: "Insights", icon: "chart" },
];
/** Pages shown as tabs under one nav item. */
const GROUPS = {
  notes: [["notes", "Notes"], ["meetings", "Meetings"]],
  library: [["dictionary", "Dictionary"], ["snippets", "Snippets"], ["actions", "Actions"]],
} as const satisfies Record<string, readonly (readonly [string, string])[]>;
type Group = keyof typeof GROUPS;
const MOD = IS_MAC ? "Super" : "Ctrl";
/** Old page ids (links, tests) still land on the right tab. */
const MOVED: Record<string, [Group, string]> = {
  scratchpad: ["notes", "notes"], notetaker: ["notes", "meetings"],
  dictionary: ["library", "dictionary"], snippets: ["library", "snippets"], transforms: ["library", "actions"],
};

export default function App() {
  const [tabs, setTabs] = useState<Record<Group, string>>({ notes: "notes", library: "dictionary" });
  const [page, setPageRaw] = useState("home");
  /** Opens a page; an old page id (or ?tab=) opens its group on the right tab. */
  const setPage = (id: string, tab?: string | null) => {
    const [group, moved] = MOVED[id] ?? [id, tab];
    if (group in GROUPS && moved) setTabs((t) => ({ ...t, [group]: moved }));
    setPageRaw(group);
  };
  useEffect(() => {
    const q = new URLSearchParams(location.search);
    setPage(q.get("page") ?? "home", q.get("tab"));
  }, []);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [onboarding, setOnboarding] = useState(false);
  const [dictation, setDictation] = useState<DictationState>({ status: "idle", mode: "dictate", handsFree: false, stage: "", micReady: false });
  const [toast, setToast] = useState<string | null>(null);
  const latest = useRef<Settings | null>(null);
  const saved = useRef<Settings | null>(null); // last state the backend accepted
  const backendVersion = useRef(0);
  const timer = useRef<number | undefined>(undefined);

  useEffect(() => {
    api.getSettings().then((s) => {
      latest.current = s;
      saved.current = s;
      setSettings(s);
      if (!s.onboarded) setOnboarding(true);
    }, (e) => setLoadError(errMsg(e)));
    // A reload while the shortcut recorder was open would leave the shortcuts paused.
    api.setShortcutRecording(false).catch(() => {});
    // A shortcut that failed to register at startup was reported before we were listening.
    api.permissions().then((p) => {
      if (p.hotkeyError) setToast(`Shortcut unavailable: ${p.hotkeyError}`);
      else checkWaylandShortcuts(p);
    }, () => {});
  }, []);

  // On Wayland the shortcuts only work once the desktop has them; say so instead of looking ready.
  const waylandNoticeShown = useRef(false);
  function checkWaylandShortcuts(p: Permissions) {
    const ws = p.waylandShortcuts;
    if (!ws || waylandNoticeShown.current || ws.gnomeShortcuts || ws.portal.state === "binding" || ws.portal.state === "bound") return;
    waylandNoticeShown.current = true;
    setToast("Shortcuts need setting up on Wayland. See Settings → Shortcuts.");
  }
  useEvent("permissions-changed", () => void api.permissions().then(checkWaylandShortcuts, () => {}));

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.altKey || e.shiftKey || (IS_MAC ? !e.metaKey || e.ctrlKey : !e.ctrlKey || e.metaKey)) return;
      if (document.querySelector("[role=dialog]")) return;
      const n = Number(/^Digit(\d)$/.exec(e.code)?.[1]);
      if (n >= 1 && n <= PAGES.length) setPage(PAGES[n - 1].id);
      else if (e.code === "Comma") setPage("settings");
      else if (e.code === "KeyF") document.querySelector<HTMLInputElement>(".panel .search input")?.focus();
      else return;
      e.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  useEffect(() => { if (settings) applyTheme(settings.theme); }, [settings?.theme]);

  useEvent("dictation-state", setDictation);
  useEvent("dictation-error", (msg) => setToast(msg));
  // A newer version found by the daily check. It is announced once per version, but only while the
  // window is showing: otherwise nobody would see the toast, and the version would be used up.
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  // Kept here so a download keeps showing its progress after leaving Settings and coming back.
  const [download, setDownload] = useState<UpdateDownload | null>(null);
  useEvent("update-download", setDownload);
  useEvent("update-available", (info) => {
    setUpdate(info);
    setDownload((d) => (d?.state === "failed" ? null : d)); // an earlier failure isn't about this result
  });
  useEffect(() => {
    if (!update) return;
    const announce = () => {
      if (document.visibilityState !== "visible") return;
      document.removeEventListener("visibilitychange", announce);
      window.removeEventListener("focus", announce);
      try {
        if (localStorage.getItem("updateNotified") === update.latest) return;
        localStorage.setItem("updateNotified", update.latest);
      } catch {}
      setToast(`OpsTrellis ${update.latest.replace(/^v/, "")} is available. See Settings → About.`);
    };
    announce();
    document.addEventListener("visibilitychange", announce);
    window.addEventListener("focus", announce);
    return () => {
      document.removeEventListener("visibilitychange", announce);
      window.removeEventListener("focus", announce);
    };
  }, [update]);
  // Changed from the tray: take the backend's copy (edits typed here in the last moment are kept).
  useEvent("settings-changed", (s) => {
    backendVersion.current++;
    saved.current = s;
    const next = latest.current ? { ...latest.current, llm: { ...latest.current.llm, enabled: s.llm.enabled } } : s;
    latest.current = next;
    setSettings(next);
  });
  useEffect(() => {
    if (!toast) return;
    const t = setTimeout(() => setToast(null), 6000);
    return () => clearTimeout(t);
  }, [toast]);

  /** Saves settings; on rejection reverts to the last accepted state so later edits aren't blocked. */
  async function persist(next: Settings) {
    const version = backendVersion.current;
    try {
      const ok = await api.saveSettings(next);
      setSaveError(null);
      // The tray changed a setting while this save was in flight, and this save overwrote it on the
      // backend: send again with the tray's change merged in (latest.current has it).
      if (backendVersion.current !== version) {
        if (latest.current) return persist(latest.current);
        return;
      }
      saved.current = ok;
      if (latest.current === next) { latest.current = ok; setSettings(ok); }
    } catch (e) {
      const msg = errMsg(e);
      setSaveError(msg);
      setToast(`Couldn't save settings: ${msg}`);
      if (latest.current === next && saved.current) { latest.current = saved.current; setSettings(saved.current); }
      throw e;
    }
  }

  // Settings edits apply locally at once and are saved after a short pause (typing in fields).
  const set: SetSetting = (key, value) => {
    const next = { ...latest.current!, [key]: value };
    latest.current = next;
    setSettings(next);
    clearTimeout(timer.current);
    timer.current = window.setTimeout(() => {
      timer.current = undefined;
      if (latest.current) persist(latest.current).catch(() => {});
    }, 400);
  };
  /** Flush pending edits now (e.g. before testing a provider). */
  const saveNow = async () => {
    clearTimeout(timer.current);
    if (latest.current) await persist(latest.current);
  };

  if (loadError) return <div className="center-msg">Could not load settings: {loadError}</div>;
  if (!settings) return <div className="center-msg">Loading…</div>;

  const props = { settings, set };
  const dark = isDarkTheme(settings.theme);
  const statusLabel = dictation.status === "recording" ? (!dictation.micReady ? "Opening mic…" : dictation.mode === "command" ? "Command…" : "Listening…")
    : dictation.status === "processing" ? (dictation.stage === "polishing" ? "Polishing…" : dictation.stage === "inserting" ? "Inserting…" : "Transcribing…")
    : dictation.status === "review" ? "Reviewing…" : "Ready";

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="sidebar-top" data-tauri-drag-region>
          <div className="brand" data-tauri-drag-region><Logo />OpsTrellis</div>
        </div>
        <nav className="nav">
          {PAGES.map((p, i) => (
            <button key={p.id} className={"nav-item" + (page === p.id ? " active" : "")} onClick={() => setPage(p.id)}
              title={`${p.label} (${comboText(`${MOD}+${i + 1}`)})`}
              aria-current={page === p.id ? "page" : undefined}>
              <Icon name={p.icon} />{p.label}
            </button>
          ))}
        </nav>
        <div className="sidebar-foot">
          <div className="foot-row">
            <button className={"nav-item" + (page === "settings" ? " active" : "")} onClick={() => setPage("settings")}
              title={`Settings (${comboText(`${MOD}+Comma`)})`}
              aria-current={page === "settings" ? "page" : undefined}>
              <Icon name="settings" />Settings
            </button>
            <button className="icon-btn" onClick={() => set("theme", dark ? "light" : "dark")}
              title={dark ? "Switch to light mode" : "Switch to dark mode"} aria-label={dark ? "Switch to light mode" : "Switch to dark mode"}>
              <Icon name={dark ? "sun" : "moon"} />
            </button>
          </div>
          <button className={"status " + dictation.status}
            title={dictation.status === "idle" ? "Start dictation" : dictation.status === "review" ? "Discard the rewrite" : "Stop dictation"}
            onClick={() => (dictation.status === "idle" ? api.startDictation() : dictation.status === "review" ? api.cancelDictation() : api.stopDictation())
              .catch((e) => setToast(errMsg(e)))}>
            <span className="status-dot" />
            <span className="status-label">{statusLabel}</span>
            {dictation.handsFree && dictation.status === "recording" && <Icon name="lock" size={12} />}
            <kbd>{comboText(settings.hotkeys.dictate)}</kbd>
          </button>
        </div>
      </aside>
      <main className="content">
        <div className="panel" key={page + (page in GROUPS ? tabs[page as Group] : "")}>
          {page === "home" && <Home {...props} onSetup={() => setOnboarding(true)} />}
          {page === "insights" && <Insights />}
          {page === "style" && <Style {...props} />}
          {page === "settings" && <SettingsPage {...props} saveError={saveError} saveNow={saveNow} update={update} download={download} />}
          {page in GROUPS && (
            <PageGroup.Provider value={{
              title: PAGES.find((p) => p.id === page)!.label,
              tabs: <Tabs label={PAGES.find((p) => p.id === page)!.label} tabs={GROUPS[page as Group].map(([id, name]) => [id, name] as [string, string])}
                value={tabs[page as Group]} onChange={(t) => setTabs({ ...tabs, [page]: t })} />,
            }}>
              {page === "notes" && (tabs.notes === "meetings" ? <Notetaker settings={settings} /> : <Scratchpad />)}
              {page === "library" && (tabs.library === "snippets" ? <Snippets /> : tabs.library === "actions" ? <Transforms {...props} /> : <Dictionary />)}
            </PageGroup.Provider>
          )}
        </div>
      </main>
      {onboarding && <Onboarding {...props} saveNow={saveNow} onClose={() => setOnboarding(false)} />}
      {toast && (
        <div className="toast" role="alert">
          <span className="toast-dot" aria-hidden="true" />
          <span className="toast-text">{toast}</span>
          <button className="icon-btn" onClick={() => setToast(null)} aria-label="Dismiss"><Icon name="x" size={16} /></button>
        </div>
      )}
    </div>
  );
}
