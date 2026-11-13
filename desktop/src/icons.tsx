// A handful of hand-drawn 24px stroke icons.
const paths = {
  home: "M3 10.5 12 3l9 7.5V20a1 1 0 0 1-1 1h-5v-6h-6v6H4a1 1 0 0 1-1-1z",
  chart: "M4 20V10M10 20V4M16 20v-7M22 20H2",
  book: "M4 4.5A1.5 1.5 0 0 1 5.5 3H20v15H5.5A1.5 1.5 0 0 0 4 19.5zM4 19.5A1.5 1.5 0 0 0 5.5 21H20v-3",
  snippet: "M8 6 3 12l5 6M16 6l5 6-5 6M13.5 4l-3 16",
  style: "M12 21a9 9 0 1 1 9-9c0 2-1.5 3-3.5 3H15a2 2 0 0 0-1.5 3.3A1.6 1.6 0 0 1 12 21zM7.5 11h.01M10.5 7h.01M15 7.5h.01",
  wand: "m4 20 11-11M14 4v2M19 9h2M17.5 6.5 19 5M11 3.5 12 5M20.5 13 19 12",
  record: "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18zM12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6z",
  note: "M5 3h10l4 4v14H5zM14 3v5h5M8 13h8M8 17h5",
  settings: "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z",
  search: "M11 18a7 7 0 1 0 0-14 7 7 0 0 0 0 14zM21 21l-5-5",
  copy: "M9 9h11v11H9zM5 15H4V4h11v1",
  trash: "M4 7h16M10 11v6M14 11v6M5 7l1 13h12l1-13M9 7V4h6v3",
  edit: "M4 20h4L19 9l-4-4L4 16zM13.5 6.5l4 4",
  plus: "M12 5v14M5 12h14",
  mic: "M12 3a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3zM5 11a7 7 0 0 0 14 0M12 18v3",
  x: "M6 6l12 12M18 6 6 18",
  stop: "M7 7h10v10H7z",
  lock: "M6 11h12v9H6zM8.5 11V8a3.5 3.5 0 0 1 7 0v3",
  check: "m5 12 5 5 9-10",
  up: "m6 15 6-6 6 6",
  down: "m6 9 6 6 6-6",
  key: "M14.5 9.5a4.5 4.5 0 1 1-1.3-3.2M13.2 6.3 20 13l-2 2-1.5-1.5L15 15l-1.5-1.5",
  flame: "M12 3c.6 3.2 4.5 5.4 4.5 10a4.5 4.5 0 0 1-9 0c0-2.3 1.2-3.7 2.3-4.6.1 1.9.9 3 2 3.3-.6-3 .2-5.8.2-8.7z",
  gauge: "M12 14.5 16 10M3.6 18a9.5 9.5 0 1 1 16.8 0",
  type: "M5 7V5h14v2M9.5 19h5M12 5v14",
  hash: "M5 9h15M4 15h15M10.5 3.5 8.5 20.5M15.5 3.5l-2 17",
  sun: "M12 16a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4",
  moon: "M20.5 14.5A8.5 8.5 0 0 1 9.5 3.5a8.5 8.5 0 1 0 11 11z",
  sparkle: "M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8zM19 16l.7 2 2 .7-2 .7-.7 2-.7-2-2-.7 2-.7z",
  undo: "M3 12a9 9 0 1 0 2.6-6.4L3 8M3 3v5h5",
} as const;

export type IconName = keyof typeof paths;

export function Icon({ name, size = 16 }: { name: IconName; size?: number }) {
  return (
    <svg className="icon" width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.75}
      strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d={paths[name]} />
    </svg>
  );
}

/** OpsTrellis's mark: white waveform bars on a solid accent square. */
export function Logo({ size = 24 }: { size?: number }) {
  return (
    <span className="brand-mark" style={{ width: size, height: size }} aria-hidden="true">
      <svg width={size * 0.625} height={size * 0.625} viewBox="0 0 20 20" fill="#fff">
        <rect x="2" y="7" width="3" height="6" rx="1.5" />
        <rect x="6.33" y="4" width="3" height="12" rx="1.5" />
        <rect x="10.67" y="2" width="3" height="16" rx="1.5" />
        <rect x="15" y="6" width="3" height="8" rx="1.5" />
      </svg>
    </span>
  );
}
