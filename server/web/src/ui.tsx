import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';

/** Loads data on mount (and whenever deps change); `reload` refetches. */
export function useAsync<T>(fn: () => Promise<T>, deps: unknown[] = []) {
  const [state, setState] = useState<{ data?: T; error?: Error; loading: boolean }>({ loading: true });
  const load = useCallback(() => {
    setState((s) => ({ ...s, loading: true }));
    fn().then(
      (data) => setState({ data, loading: false }),
      (error) => setState({ error, loading: false }),
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  useEffect(load, [load]);
  return { ...state, reload: load };
}

/* ---------- icons (24px stroke set, drawn for OpsTrellis) ---------- */

const ICONS = {
  overview: (
    <>
      <rect x="3" y="3" width="7.5" height="9" rx="2" />
      <rect x="13.5" y="3" width="7.5" height="5" rx="2" />
      <rect x="13.5" y="11" width="7.5" height="10" rx="2" />
      <rect x="3" y="15" width="7.5" height="6" rx="2" />
    </>
  ),
  billing: (
    <>
      <rect x="2.5" y="5" width="19" height="14" rx="3" />
      <path d="M2.5 10h19M6.5 15h3" />
    </>
  ),
  devices: (
    <>
      <rect x="4" y="4.5" width="16" height="11" rx="2" />
      <path d="M2 19.5h20" />
    </>
  ),
  settings: <path d="M20 5h-6M10 5H4M20 12h-9M7 12H4M20 19h-4M12 19H4M14 3v4M7 10v4M16 17v4" />,
  revenue: <path d="M3 17l6-6 4 4 8-8M15 7h6v6" />,
  users: (
    <>
      <circle cx="9" cy="8" r="3.5" />
      <path d="M2.5 20v-1a5 5 0 0 1 5-5h3a5 5 0 0 1 5 5v1M16 4.5a3.5 3.5 0 0 1 0 7M21.5 20v-1a5 5 0 0 0-3.5-4.8" />
    </>
  ),
  team: (
    <>
      <circle cx="12" cy="7.5" r="3" />
      <path d="M6.5 20v-1.5a5.5 5.5 0 0 1 11 0V20" />
      <circle cx="5" cy="10" r="2" />
      <circle cx="19" cy="10" r="2" />
      <path d="M2 18.5V18a3.5 3.5 0 0 1 3-3.46M22 18.5V18a3.5 3.5 0 0 0-3-3.46" />
    </>
  ),
  models: <path d="M12 2.5l9.5 4.75L12 12 2.5 7.25zM2.5 12L12 16.75 21.5 12M2.5 16.75L12 21.5l9.5-4.75" />,
  key: (
    <>
      <circle cx="7.5" cy="15.5" r="4.5" />
      <path d="M10.7 12.3L20 3M16.5 6.5l3 3M14 9l2 2" />
    </>
  ),
  logout: <path d="M9 20H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h3M16 16l4-4-4-4M20 12H9" />,
  close: <path d="M17 7L7 17M7 7l10 10" />,
  words: <path d="M4 7V5h16v2M12 5v14M9 19h6" />,
  speed: <path d="M12 14l4-4M3.5 18.5a9.5 9.5 0 1 1 17 0" />,
  flame: <path d="M12 21a6.5 6.5 0 0 0 6.5-6.5c0-3.5-2.5-6-4-8.5-.5 2-1.5 3-3 3.5.5-2.5-.5-5-3-7 0 4-4 6-4 12A6.5 6.5 0 0 0 12 21z" />,
  coins: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M15 9.2A2.6 2.6 0 0 0 12.6 8h-1.2a2 2 0 0 0 0 4h1.2a2 2 0 0 1 0 4h-1.2A2.6 2.6 0 0 1 9 14.8M12 6.5V8M12 16v1.5" />
    </>
  ),
  star: <path d="M12 3l2.6 5.6 6.1.7-4.5 4.2 1.2 6L12 16.5 6.6 19.5l1.2-6-4.5-4.2 6.1-.7z" />,
  clock: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 7v5l3 2" />
    </>
  ),
  plus: <path d="M12 5v14M5 12h14" />,
  check: <path d="M5 12.5l4.5 4.5L19 7.5" />,
  copy: (
    <>
      <rect x="8" y="8" width="12" height="12" rx="2.5" />
      <path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2" />
    </>
  ),
} as const;
export type IconName = keyof typeof ICONS;

export function Icon({ name, size = 18 }: { name: IconName; size?: number }) {
  return (
    <svg className="icon" width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.8} strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      {ICONS[name]}
    </svg>
  );
}

/** The OpsTrellis mark: a solid accent square with white waveform bars. */
export function Logo({ size = 24 }: { size?: number }) {
  return (
    <svg className="logo" width={size} height={size} viewBox="0 0 24 24" aria-hidden>
      <rect width="24" height="24" rx="6" fill="var(--accent)" />
      <g stroke="#fff" strokeWidth="2" strokeLinecap="round">
        <path d="M6 10.5v3M9.5 8v8M13 6v12M16.5 9v6M20 11v2" />
      </g>
    </svg>
  );
}

/* ---------- layout ---------- */

export function Page({ title, subtitle, actions, children }: { title: string; subtitle?: ReactNode; actions?: ReactNode; children: ReactNode }) {
  return (
    <div className="page">
      <header className="page-head">
        <div className="col page-head-inner">
          <h1>{title}</h1>
          {actions && <div className="page-actions">{actions}</div>}
        </div>
      </header>
      <div className="col page-body">
        {subtitle && <p className="page-desc">{subtitle}</p>}
        {children}
      </div>
    </div>
  );
}

export function Card({ title, actions, children, className = '' }: { title?: ReactNode; actions?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <section className={`card ${className}`}>
      {(title || actions) && (
        <div className="card-head">
          {title && <h2>{title}</h2>}
          {actions}
        </div>
      )}
      {children}
    </section>
  );
}

/** One card with equal columns separated by hairlines. */
export function StatStrip({ children }: { children: ReactNode }) {
  return <div className="stat-strip">{children}</div>;
}

export function Stat({ label, value, hint }: { label: string; value: ReactNode; hint?: ReactNode }) {
  return (
    <div className="stat">
      <div className="stat-label">{label}</div>
      <div className="stat-value">{value}</div>
      {hint && <div className="stat-hint">{hint}</div>}
    </div>
  );
}

export function Loading({ error }: { error?: Error }) {
  return error ? (
    <p className="error">{error.message}</p>
  ) : (
    <div className="loading" role="status" aria-label="Loading">
      <span />
      <span />
      <span />
      <span />
      <span />
    </div>
  );
}

export function ErrorText({ error }: { error: string | null }) {
  return error ? <p className="error">{error}</p> : null;
}

export function Meter({ value, max }: { value: number; max: number }) {
  const pct = max > 0 ? Math.min(100, (value / max) * 100) : 0;
  return (
    <div className="meter" role="progressbar" aria-valuenow={value} aria-valuemax={max}>
      <div className={`meter-fill ${pct >= 100 ? 'full' : ''}`} style={{ width: `${pct}%` }} />
    </div>
  );
}

export interface Series {
  name: string;
  color: string; // a solid CSS colour, e.g. var(--series-1)
}

/**
 * Vertical bar chart. One series -> no legend (the card title names it); several -> grouped bars
 * with a legend. Hovering a column shows its values.
 */
export function BarChart({ data, series, format = String, height = 180, empty = 'No data yet.' }: { data: { label: string; values: number[] }[]; series: Series[]; format?: (n: number) => string; height?: number; empty?: string }) {
  const [hover, setHover] = useState<number | null>(null);
  const max = Math.max(0, ...data.flatMap((d) => d.values));
  if (!(max > 0)) return <p className="muted empty chart-empty">{empty}</p>;
  const h = hover != null ? data[hover] : null;
  return (
    <div className="chart">
      {series.length > 1 && (
        <div className="legend">
          {series.map((s) => (
            <span key={s.name}>
              <i style={{ background: s.color }} />
              {s.name}
            </span>
          ))}
        </div>
      )}
      <div className="chart-plot" style={{ height }} onMouseLeave={() => setHover(null)}>
        <div className="chart-grid" aria-hidden>
          <span data-v={format(max)} />
          <span data-v={format(max / 2)} />
          <span />
        </div>
        {data.map((d, i) => (
          <div key={d.label} className={`chart-col ${hover === i ? 'hover' : ''}`} onMouseEnter={() => setHover(i)}>
            {d.values.map((v, j) => (
              <div key={j} className="chart-bar" style={{ height: `${(v / max) * 100}%`, background: series[j]!.color }} />
            ))}
          </div>
        ))}
        {h && (
          <div className="tooltip" style={{ left: `clamp(70px, ${((hover! + 0.5) / data.length) * 100}%, calc(100% - 70px))` }}>
            <strong>{h.label}</strong>
            {series.map((s, j) => (
              <div key={s.name}>
                {series.length > 1 && <i style={{ background: s.color }} />}
                {series.length > 1 ? `${s.name}: ` : ''}
                {format(h.values[j]!)}
              </div>
            ))}
          </div>
        )}
      </div>
      <div className="chart-axis">
        <span>{data[0]!.label}</span>
        {data.length > 1 && <span>{data[data.length - 1]!.label}</span>}
      </div>
    </div>
  );
}

export function Modal({ title, onClose, children }: { title: string; onClose: () => void; children: ReactNode }) {
  useEffect(() => {
    const on = (e: KeyboardEvent) => e.key === 'Escape' && onClose();
    addEventListener('keydown', on);
    return () => removeEventListener('keydown', on);
  }, [onClose]);
  // Portalled to <body>: the sticky glass header (backdrop-filter) would otherwise become the containing block of the fixed scrim.
  return createPortal(
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()} role="dialog" aria-label={title}>
        <div className="card-head">
          <h2>{title}</h2>
          <button className="icon-btn" onClick={onClose} aria-label="Close">
            <Icon name="close" />
          </button>
        </div>
        {children}
      </div>
    </div>,
    document.body,
  );
}

/** A secret shown once, with Copy. Without the Clipboard API (plain-HTTP origins) the text is selected for manual copying. */
export function SecretCopy({ value, noun = 'code' }: { value: string; noun?: string }) {
  const ref = useRef<HTMLElement>(null);
  const [state, setState] = useState<'idle' | 'copied' | 'manual'>('idle');
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(value);
      setState('copied');
    } catch {
      const range = document.createRange();
      range.selectNodeContents(ref.current!);
      getSelection()?.removeAllRanges();
      getSelection()?.addRange(range);
      setState('manual');
    }
  };
  return (
    <>
      <div className="row">
        <code ref={ref} className="secret">
          {value}
        </code>
        <button onClick={copy}>
          <Icon name={state === 'copied' ? 'check' : 'copy'} size={16} />
          {state === 'copied' ? 'Copied' : 'Copy'}
        </button>
      </div>
      {state === 'manual' && <p className="muted small">Select and copy the {noun}.</p>}
    </>
  );
}
