import { useState, type FormEvent } from 'react';
import { api, date, mins, num, price, usd, type Me } from '../api';
import { Link, navigate } from '../router';
import { BarChart, Card, ErrorText, Icon, Loading, Meter, Modal, Page, SecretCopy, Stat, StatStrip, useAsync } from '../ui';

const tz = Intl.DateTimeFormat().resolvedOptions().timeZone;

export function Overview({ me }: { me: Me }) {
  const stats = useAsync(() => api(`/api/stats?tz=${encodeURIComponent(tz)}`));
  const s = stats.data;
  const daily = (() => {
    if (!s) return [];
    const byDay = new Map<string, number>(s.daily.map((d: any) => [d.day, d.words]));
    const out = [];
    for (let i = 29; i >= 0; i--) {
      const d = new Date(Date.now() - i * 86_400_000).toLocaleDateString('en-CA', { timeZone: tz });
      out.push({ label: d, values: [byDay.get(d) ?? 0] });
    }
    return out;
  })();
  const u = me.usage;
  return (
    <Page title={`Hi${me.user.name ? `, ${me.user.name}` : ''}`} subtitle="Your dictation at a glance.">
      {!s ? (
        <Loading error={stats.error} />
      ) : (
        <>
          <StatStrip>
            <Stat label="Words dictated" value={num(s.totals.words)} hint={`${num(s.totals.dictations)} dictations`} />
            <Stat label="Average speed" value={<>{num(s.totals.avg_wpm)}<small> WPM</small></>} hint={`${num(s.totals.audio_minutes, 1)} min of audio`} />
            <Stat label="Current streak" value={<>{s.streak.current}<small> {s.streak.current === 1 ? 'day' : 'days'}</small></>} hint={`Longest: ${s.streak.longest} ${s.streak.longest === 1 ? 'day' : 'days'}`} />
            <Stat label="Provider cost" value={usd(s.totals.cost_usd)} hint="All time, incl. your own keys" />
          </StatStrip>
          <div className="grid wide">
            <Card title="Words per day" actions={<span className="card-meta">Last 30 days</span>}>
              <BarChart data={daily} series={[{ name: 'Words', color: 'var(--series-1)' }]} format={(n) => num(n)} empty="No dictation in the last 30 days." />
            </Card>
            <Card title="OpsTrellis Cloud minutes" actions={<span className="card-meta">This period</span>} className="minutes">
              <div className="minutes-figure">
                <span className="stat-value">{num(u.managed_minutes, 1)}</span>
                <span className="muted"> of {num(u.included_minutes)} min</span>
              </div>
              <Meter value={u.managed_minutes} max={u.included_minutes} />
              <dl className="facts">
                <div>
                  <dt>Remaining</dt>
                  <dd>{num(u.remaining_minutes, 1)} min</dd>
                </div>
                <div>
                  <dt>Period</dt>
                  <dd>
                    {date(me.period.start)} – {date(me.period.end)}
                  </dd>
                </div>
                {u.overage_minutes > 0 && (
                  <div>
                    <dt>Overage</dt>
                    <dd>
                      {num(u.overage_minutes)} of {num(me.limits.max_overage_minutes)} min ({usd(u.cost_usd)})
                    </dd>
                  </div>
                )}
              </dl>
            </Card>
          </div>
          <div className="grid two">
            <Card title="By app category">
              {s.by_app_category.length ? (
                <table>
                  <thead>
                    <tr>
                      <th>Category</th>
                      <th className="num">Words</th>
                      <th className="num">Minutes</th>
                    </tr>
                  </thead>
                  <tbody>
                    {s.by_app_category.map((a: any) => (
                      <tr key={a.app_category}>
                        <td className="cap">{a.app_category}</td>
                        <td className="num">{num(a.words)}</td>
                        <td className="num">{mins(a.minutes)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              ) : (
                <p className="muted empty">Synced from the desktop app once you dictate.</p>
              )}
            </Card>
            <Card title="By provider & model">
              {s.by_model.length ? (
                <table className="stack">
                  <thead>
                    <tr>
                      <th>Model</th>
                      <th>Mode</th>
                      <th className="num">Usage</th>
                      <th className="num">Cost</th>
                    </tr>
                  </thead>
                  <tbody>
                    {s.by_model.map((m: any) => (
                      <tr key={`${m.model}-${m.mode}`}>
                        <td>
                          <code>{m.model.includes('/') ? m.model : `${m.provider}/${m.model}`}</code>
                        </td>
                        <td>
                          <span className={`pill ${m.mode}`}>{m.mode}</span>
                        </td>
                        <td className="num" data-label="Usage">{m.tokens ? `${num(m.tokens)} tok` : `${mins(m.minutes)} min`}</td>
                        <td className="num" data-label="Cost">{usd(m.cost_usd)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              ) : (
                <p className="muted empty">No usage yet.</p>
              )}
            </Card>
          </div>
        </>
      )}
    </Page>
  );
}

export function Billing({ me }: { me: Me }) {
  const billing = useAsync(() => api('/api/billing'));
  const [error, setError] = useState<string | null>(null);
  const status = new URLSearchParams(location.search).get('checkout');
  const go = (path: string) => async () => {
    setError(null);
    try {
      location.href = (await api<{ url: string }>(path, { method: 'POST' })).url;
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const b = billing.data;
  if (!b) return <Loading error={billing.error} />;
  const manual = b.mode === 'manual';
  return (
    <Page title="Plan & billing" subtitle="Bring-your-own-key and local models are always free and unlimited.">
      {status === 'success' && <p className="notice">Thanks! Your Pro plan is being activated.</p>}
      {manual && <p className="notice">Billing is handled manually on this server. Ask an administrator to change your plan.</p>}
      <ErrorText error={error} />
      <div className="grid two">
        {(['free', 'pro'] as const).map((id) => {
          const p = b.plans[id];
          const current = me.plan === id;
          return (
            <Card key={id} className={`plan plan-${id} ${current ? 'current' : ''}`} title={p.name} actions={current && <span className="pill accent">Current plan</span>}>
              <div className="price">
                <span className="price-amount">${p.priceUsd}</span>
                <span className="muted"> / month</span>
              </div>
              <ul className="features">
                <li>
                  <Icon name="check" size={16} />
                  {p.includedMinutes} OpsTrellis Cloud minutes per month
                </li>
                <li>
                  <Icon name="check" size={16} />
                  {p.overagePerMinUsd && b.max_overage_minutes > 0
                    ? `Then $${p.overagePerMinUsd.toFixed(2)} per extra minute, up to ${num(b.max_overage_minutes)} extra minutes`
                    : 'Hard limit, no surprise bills'}
                </li>
                <li>
                  <Icon name="check" size={16} />
                  Unlimited BYOK and local transcription
                </li>
              </ul>
              {!manual && id === 'pro' && !current && (
                <button className="primary" onClick={go('/api/billing/checkout')}>
                  Upgrade to Pro
                </button>
              )}
              {!manual && current && b.has_customer && (
                <button onClick={go('/api/billing/portal')}>Manage subscription</button>
              )}
              {current && me.plan_status !== 'active' && <p className="error small">Subscription status: {me.plan_status}</p>}
            </Card>
          );
        })}
      </div>
    </Page>
  );
}

export function Tokens() {
  const tokens = useAsync(() => api('/api/tokens'));
  const [name, setName] = useState('');
  const [created, setCreated] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const create = async () => {
    setError(null);
    try {
      setCreated((await api<{ token: string }>('/api/tokens', { json: { name: name || 'API token' } })).token);
      setName('');
      tokens.reload();
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const revoke = async (id: string) => {
    if (!confirm('Revoke this token? Apps using it will be signed out.')) return;
    await api(`/api/tokens/${id}`, { method: 'DELETE' });
    tokens.reload();
  };
  return (
    <Page title="Devices & tokens" subtitle="Desktop apps you sign in appear here. Tokens also work as an OpenAI API key with base URL /v1.">
      <Card title="Create a token" className="create-token">
        <div className="row">
          <input placeholder="Name, e.g. CI script" value={name} onChange={(e) => setName(e.target.value)} />
          <button className="primary" onClick={create}>
            <Icon name="plus" size={16} />
            Create
          </button>
        </div>
        <ErrorText error={error} />
      </Card>
      <Card title="Active tokens">
        {!tokens.data ? (
          <Loading error={tokens.error} />
        ) : tokens.data.tokens.length === 0 ? (
          <p className="muted empty">No tokens yet. Sign in from the desktop app or create one above.</p>
        ) : (
          <table className="stack">
            <thead>
              <tr>
                <th>Name</th>
                <th>Token</th>
                <th>Last used</th>
                <th>Created</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {tokens.data.tokens.map((t: any) => (
                <tr key={t.id}>
                  <td>
                    <span className="cell-title">
                      <span className="cell-icon">
                        <Icon name="key" size={16} />
                      </span>
                      {t.name}
                    </span>
                  </td>
                  <td>
                    <code>{t.prefix}…</code>
                  </td>
                  <td className="nowrap" data-label="Last used">
                    {t.last_used_at ? date(t.last_used_at) : 'Never'}
                  </td>
                  <td className="nowrap" data-label="Created">
                    {date(t.created_at)}
                  </td>
                  <td className="num">
                    <button className="ghost danger" onClick={() => revoke(t.id)}>
                      Revoke
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Card>
      {created && (
        <Modal title="Your new token" onClose={() => setCreated(null)}>
          <p className="muted">Copy it now. It will not be shown again.</p>
          <SecretCopy value={created} noun="token" />
        </Modal>
      )}
    </Page>
  );
}

export function Settings({ me, onSaved }: { me: Me; onSaved: () => void }) {
  const catalog = useAsync(() => api('/api/catalog'));
  const [form, setForm] = useState({ name: me.user.name, preferred_model: me.preferred_model ?? '', preferred_llm: me.preferred_llm ?? '' });
  const [msg, setMsg] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const save = async () => {
    setError(null);
    try {
      await api('/api/me', { method: 'PATCH', json: { name: form.name, preferred_model: form.preferred_model || null, preferred_llm: form.preferred_llm || null } });
      setMsg('Saved');
      onSaved();
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const options = (kind: string) =>
    (catalog.data?.models ?? [])
      .filter((m: any) => m.kind === kind)
      .map((m: any) => (
        <option key={m.id} value={m.id}>
          {m.display_name}
          {kind === 'stt' ? ` · ${m.price_per_min_usd ? `${price(m.price_per_min_usd)}/min` : 'free'}` : ` · ${price(m.input_per_1m_usd)} / ${price(m.output_per_1m_usd)} per 1M tokens`}
          {m.streaming ? ' · streaming' : ''}
        </option>
      ));
  return (
    <Page title="Settings" subtitle="These apply when the desktop app or an API call uses the “auto” model on OpsTrellis Cloud.">
      <Card title="Profile & managed models" className="narrow">
        <div className="form">
          <label>
            Name
            <input value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} />
          </label>
          <label>
            Speech-to-text model for “auto”
            <select value={form.preferred_model} onChange={(e) => setForm({ ...form, preferred_model: e.target.value })}>
              <option value="">Server default (fallback order)</option>
              {options('stt')}
            </select>
          </label>
          <label>
            Cleanup LLM for “auto”
            <select value={form.preferred_llm} onChange={(e) => setForm({ ...form, preferred_llm: e.target.value })}>
              <option value="">Server default (fallback order)</option>
              {options('llm')}
            </select>
          </label>
          <ErrorText error={error} />
          <div className="row">
            <button className="primary" onClick={save}>
              Save
            </button>
            {msg && <span className="muted">{msg}</span>}
          </div>
        </div>
      </Card>
      <ChangePassword />
    </Page>
  );
}

function ChangePassword() {
  const [form, setForm] = useState({ current: '', next: '' });
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    setMsg(null);
    try {
      await api('/api/auth/password', { json: form });
      setForm({ current: '', next: '' });
      setMsg('Password changed');
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Card title="Change password" className="narrow">
      <form className="form" onSubmit={submit}>
        <label>
          Current password
          <input type="password" required autoComplete="current-password" value={form.current} onChange={(e) => setForm({ ...form, current: e.target.value })} />
        </label>
        <label>
          New password
          <input type="password" required minLength={8} autoComplete="new-password" placeholder="At least 8 characters" value={form.next} onChange={(e) => setForm({ ...form, next: e.target.value })} />
        </label>
        <p className="muted small">
          Other browsers are signed out. Desktop apps and API tokens stay signed in; revoke them under <Link href="/tokens">Devices &amp; tokens</Link>.
        </p>
        <ErrorText error={error} />
        <div className="row">
          <button className="primary" disabled={busy}>
            Change password
          </button>
          {msg && <span className="muted">{msg}</span>}
        </div>
      </form>
    </Card>
  );
}

export function Device({ code }: { code: string }) {
  const info = useAsync(() => (code ? api(`/api/device/${encodeURIComponent(code)}`) : Promise.reject(new Error('No code in the link.'))), [code]);
  const [done, setDone] = useState<'approved' | 'denied' | null>(null);
  const [error, setError] = useState<string | null>(null);
  const act = (what: 'approve' | 'deny') => async () => {
    try {
      await api(`/api/device/${what}`, { json: { user_code: code } });
      setDone(what === 'approve' ? 'approved' : 'denied');
    } catch (e) {
      setError((e as Error).message);
    }
  };
  return (
    <Page title="Connect a device">
      <Card className="narrow device-card">
        <span className={`device-icon ${done === 'approved' ? 'done' : ''}`} aria-hidden>
          <Icon name={done === 'approved' ? 'check' : 'devices'} size={20} />
        </span>
        {done === 'approved' ? (
          <>
            <h2>You're connected</h2>
            <p className="muted">Return to the OpsTrellis app. It will finish signing in automatically.</p>
          </>
        ) : done === 'denied' ? (
          <p>Request denied. Nothing was connected.</p>
        ) : info.error ? (
          <>
            <p className="error">{info.error.message}</p>
            <p className="muted">Start signing in again from the OpsTrellis app to get a new link.</p>
          </>
        ) : !info.data ? (
          <Loading />
        ) : (
          <>
            <p>
              <strong>{info.data.device_name}</strong> wants to use your OpsTrellis Cloud account.
            </p>
            <p className="muted">Check that the app shows this code:</p>
            <div className="user-code" aria-label={info.data.user_code}>
              {String(info.data.user_code)
                .split('')
                .map((ch: string, i: number) => (ch === '-' ? <span key={i} className="sep" aria-hidden>–</span> : <span key={i} aria-hidden>{ch}</span>))}
            </div>
            <ErrorText error={error} />
            <div className="row">
              <button className="primary" onClick={act('approve')}>
                Approve
              </button>
              <button onClick={act('deny')}>Deny</button>
              <button className="ghost" onClick={() => navigate('/')}>
                Cancel
              </button>
            </div>
          </>
        )}
      </Card>
    </Page>
  );
}
