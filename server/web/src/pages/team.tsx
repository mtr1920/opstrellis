import { useState } from 'react';
import { api, date, num } from '../api';
import { Card, ErrorText, Icon, Loading, Modal, Page, SecretCopy, Stat, StatStrip, useAsync } from '../ui';

type Role = 'admin' | 'member';
interface TeamInfo {
  id: string;
  name: string;
  role: Role;
  members: number;
  libraryVersion: number;
}
type Kind = 'dictionary' | 'snippet' | 'action';
interface Entry {
  id: string;
  kind: Kind;
  data: Record<string, string | null>;
  updatedAt: string;
}

const errorMessage = (e: unknown) => (e as Error).message;

export function Team({ meId }: { meId: string }) {
  const team = useAsync(() => api<{ team: TeamInfo | null }>('/api/team'));
  if (!team.data) return <Loading error={team.error} />;
  const t = team.data.team;
  return t ? <TeamHome team={t} meId={meId} onChange={team.reload} /> : <NoTeam onJoined={team.reload} />;
}

const SUBTITLE = 'Share a dictionary, snippets and voice actions with your team. Members’ desktop apps sync them automatically.';

function NoTeam({ onJoined }: { onJoined: () => void }) {
  const [name, setName] = useState('');
  const [code, setCode] = useState('');
  const [error, setError] = useState<{ create?: string; join?: string }>({});
  const [busy, setBusy] = useState(false);
  const submit = (which: 'create' | 'join') => async () => {
    if (busy) return;
    setError({});
    if (which === 'create' && !name.trim()) return setError({ create: 'Enter a team name.' });
    if (which === 'join' && !code.trim()) return setError({ join: 'Enter the invite code you were given.' });
    setBusy(true);
    try {
      await (which === 'create' ? api('/api/team', { json: { name } }) : api('/api/team/join', { json: { code } }));
      onJoined();
    } catch (e) {
      setError({ [which]: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };
  return (
    <Page title="Team" subtitle={SUBTITLE}>
      <div className="grid two">
        <Card title="Create a team">
          <div className="form">
            <p className="muted">You become its admin and can invite people with a code.</p>
            <div className="row">
              <input aria-label="Team name" placeholder="Team name" maxLength={80} value={name} onChange={(e) => setName(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && submit('create')()} />
              <button className="primary" disabled={busy} onClick={submit('create')}>
                <Icon name="plus" size={16} />
                Create
              </button>
            </div>
            <ErrorText error={error.create ?? null} />
          </div>
        </Card>
        <Card title="Join a team">
          <div className="form">
            <p className="muted">Ask a team admin for an invite code.</p>
            <div className="row">
              <input aria-label="Invite code" placeholder="Invite code" autoComplete="off" spellCheck={false} value={code} onChange={(e) => setCode(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && submit('join')()} />
              <button disabled={busy} onClick={submit('join')}>
                Join
              </button>
            </div>
            <ErrorText error={error.join ?? null} />
          </div>
        </Card>
      </div>
    </Page>
  );
}

function TeamHome({ team, meId, onChange }: { team: TeamInfo; meId: string; onChange: () => void }) {
  const isAdmin = team.role === 'admin';
  const [error, setError] = useState<string | null>(null);
  const leave = async () => {
    const only = team.members === 1;
    if (!confirm(only ? `You are the only member. Leaving deletes “${team.name}” and its library. Continue?` : `Leave “${team.name}”? Its library will stop syncing to your devices, and invite codes you created stop working.`)) return;
    setError(null);
    try {
      await api('/api/team/leave', { method: 'POST' });
      onChange();
    } catch (e) {
      setError(errorMessage(e));
    }
  };
  return (
    <Page title="Team" subtitle={SUBTITLE} actions={<button onClick={leave}>Leave team</button>}>
      <ErrorText error={error} />
      <StatStrip>
        <Stat label="Team" value={<span className="truncate" title={team.name}>{team.name}</span>} hint={`${num(team.members)} ${team.members === 1 ? 'member' : 'members'}`} />
        <Stat label="Your role" value={<span className="cap">{team.role}</span>} hint={isAdmin ? 'You manage members and the library' : 'The library is read-only for you'} />
      </StatStrip>
      <Members isAdmin={isAdmin} meId={meId} onChange={onChange} />
      {isAdmin && <Invites />}
      <Library editable={isAdmin} />
    </Page>
  );
}

// ---------- members ----------

function Members({ isAdmin, meId, onChange }: { isAdmin: boolean; meId: string; onChange: () => void }) {
  const list = useAsync(() => api<{ members: { id: string; email: string; name: string; role: Role }[] }>('/api/team/members'));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const act = async (fn: () => Promise<unknown>) => {
    if (busy) return;
    setError(null);
    setBusy(true);
    try {
      await fn();
      list.reload();
      onChange();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const setRole = (id: string, role: Role) => {
    if (id === meId && role === 'member' && !confirm('Make yourself a member? You will no longer manage the team, and the invite codes you created are revoked.')) return list.reload();
    return act(() => api(`/api/team/members/${id}`, { method: 'PATCH', json: { role } }));
  };
  const remove = (id: string, email: string) =>
    confirm(`Remove ${email} from the team? Invite codes are revoked; create a new one to invite people.`) && act(() => api(`/api/team/members/${id}`, { method: 'DELETE' }));
  return (
    <Card title="Members">
      <ErrorText error={error} />
      {!list.data ? (
        <Loading error={list.error} />
      ) : (
        <table className="stack">
          <thead>
            <tr>
              <th>Member</th>
              <th>Role</th>
              {isAdmin && <th />}
            </tr>
          </thead>
          <tbody>
            {list.data.members.map((m) => (
              <tr key={m.id}>
                <td>
                  <span className="cell-title">
                    <span className="avatar sm" aria-hidden>
                      {(m.name || m.email).trim().charAt(0).toUpperCase()}
                    </span>
                    <span>
                      <div>
                        {m.email}
                        {m.id === meId && <span className="muted"> (you)</span>}
                      </div>
                      {m.name && <div className="muted small">{m.name}</div>}
                    </span>
                  </span>
                </td>
                <td>
                  {isAdmin ? (
                    <select aria-label={`Role of ${m.email}`} value={m.role} disabled={busy} onChange={(e) => setRole(m.id, e.target.value as Role)}>
                      <option value="member">member</option>
                      <option value="admin">admin</option>
                    </select>
                  ) : (
                    <span className={`pill ${m.role === 'admin' ? 'accent' : ''}`}>{m.role}</span>
                  )}
                </td>
                {isAdmin && (
                  <td className="num">
                    {m.id !== meId && (
                      <button className="ghost danger" disabled={busy} onClick={() => remove(m.id, m.email)}>
                        Remove
                      </button>
                    )}
                  </td>
                )}
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </Card>
  );
}

// ---------- invites ----------

function Invites() {
  const list = useAsync(() => api<{ invites: { id: string; expiresAt: string; createdAt: string }[] }>('/api/team/invites'));
  const [created, setCreated] = useState<{ code: string; expiresAt: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const create = async () => {
    if (busy) return;
    setError(null);
    setBusy(true);
    try {
      setCreated(await api('/api/team/invites', { method: 'POST' }));
      list.reload();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const revoke = async (id: string) => {
    if (busy || !confirm('Revoke this invite code? Nobody can join with it any more.')) return;
    setError(null);
    setBusy(true);
    try {
      await api(`/api/team/invites/${id}`, { method: 'DELETE' });
      list.reload();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Card
      title="Invite codes"
      actions={
        <button className="primary" disabled={busy} onClick={create}>
          <Icon name="plus" size={16} />
          New code
        </button>
      }
    >
      <ErrorText error={error} />
      {!list.data ? (
        <Loading error={list.error} />
      ) : list.data.invites.length === 0 ? (
        <p className="muted empty">No active codes. Anyone with a code can join until it expires 7 days after it is made.</p>
      ) : (
        <table className="stack">
          <thead>
            <tr>
              <th>Code</th>
              <th>Created</th>
              <th>Expires</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {list.data.invites.map((i) => (
              <tr key={i.id}>
                <td>
                  <span className="cell-title">
                    <span className="cell-icon">
                      <Icon name="key" size={16} />
                    </span>
                    <span className="muted">Hidden after creation</span>
                  </span>
                </td>
                <td className="nowrap" data-label="Created">
                  {date(i.createdAt)}
                </td>
                <td className="nowrap" data-label="Expires">
                  {date(i.expiresAt)}
                </td>
                <td className="num">
                  <button className="ghost danger" disabled={busy} onClick={() => revoke(i.id)}>
                    Revoke
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {created && (
        <Modal title="New invite code" onClose={() => setCreated(null)}>
          <p className="muted">Share it with the people you want to invite. It works until {date(created.expiresAt)} and will not be shown again.</p>
          <SecretCopy value={created.code} />
        </Modal>
      )}
    </Card>
  );
}

// ---------- library ----------

interface Field {
  key: string;
  label: string;
  max: number;
  optional?: boolean;
  multiline?: boolean;
  options?: [value: string, label: string][];
}
const KINDS: Record<Kind, { tab: string; noun: string; fields: Field[]; empty: string }> = {
  dictionary: {
    tab: 'Dictionary',
    noun: 'word',
    empty: 'No words yet. Add names and jargon so dictation spells them right.',
    fields: [
      { key: 'phrase', label: 'Word or phrase', max: 100 },
      { key: 'replacement', label: 'Replace with (optional)', max: 200, optional: true },
    ],
  },
  snippet: {
    tab: 'Snippets',
    noun: 'snippet',
    empty: 'No snippets yet. Say a trigger to insert longer text.',
    fields: [
      { key: 'trigger', label: 'Trigger', max: 100 },
      { key: 'expansion', label: 'Expands to', max: 5000, multiline: true },
    ],
  },
  action: {
    tab: 'Actions',
    noun: 'action',
    empty: 'No voice actions yet. An action runs a prompt on what you say.',
    fields: [
      { key: 'name', label: 'Name', max: 60 },
      { key: 'prompt', label: 'Prompt', max: 2000, multiline: true },
      {
        key: 'output',
        label: 'Result',
        max: 10,
        options: [
          ['replace', 'Replaces the text'],
          ['copy', 'Copied to the clipboard'],
        ],
      },
    ],
  },
};
const ORDER = Object.keys(KINDS) as Kind[];

function blank(kind: Kind): Record<string, string> {
  return Object.fromEntries(KINDS[kind].fields.map((f) => [f.key, f.options ? f.options[0]![0] : '']));
}

/** Field errors as the server would report them, so most mistakes show before saving. */
function validate(kind: Kind, form: Record<string, string>) {
  const errors: Record<string, string> = {};
  for (const f of KINDS[kind].fields) {
    const v = (form[f.key] ?? '').trim();
    if (!v && !f.optional) errors[f.key] = 'Required';
    else if (v.length > f.max) errors[f.key] = `At most ${num(f.max)} characters (now ${num(v.length)})`;
  }
  return errors;
}

function Library({ editable }: { editable: boolean }) {
  const lib = useAsync(() => api<{ version: number; entries: Entry[] }>('/api/team/library'));
  const [kind, setKind] = useState<Kind>('dictionary');
  const [editing, setEditing] = useState<{ id: string | null; form: Record<string, string> } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const k = KINDS[kind];
  const entries = (lib.data?.entries ?? [])
    .filter((e) => e.kind === kind)
    .sort((a, b) => String(a.data[k.fields[0]!.key]).localeCompare(String(b.data[k.fields[0]!.key])));

  const remove = async (e: Entry) => {
    if (!confirm(`Delete “${e.data[k.fields[0]!.key]}”? It is removed from every member’s app at the next sync.`)) return;
    setError(null);
    try {
      await api(`/api/team/library/${e.id}`, { method: 'DELETE' });
      lib.reload();
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  return (
    <Card
      title="Library"
      actions={
        editable && (
          <button className="primary" onClick={() => setEditing({ id: null, form: blank(kind) })}>
            <Icon name="plus" size={16} />
            Add {k.noun}
          </button>
        )
      }
    >
      <div className="tabs" role="tablist" aria-label="Library sections">
        {ORDER.map((id) => (
          <button key={id} role="tab" className="ghost" aria-selected={id === kind} onClick={() => setKind(id)}>
            {KINDS[id].tab}
            {lib.data && <span className="tab-count">{lib.data.entries.filter((e) => e.kind === id).length}</span>}
          </button>
        ))}
      </div>
      <ErrorText error={error} />
      {!lib.data ? (
        <Loading error={lib.error} />
      ) : entries.length === 0 ? (
        <p className="muted empty">{editable ? k.empty : 'Nothing here yet. Team admins add entries.'}</p>
      ) : (
        <table className="stack" role="tabpanel">
          <thead>
            <tr>
              {k.fields.map((f) => (
                <th key={f.key}>{f.label.replace(' (optional)', '')}</th>
              ))}
              {editable && <th />}
            </tr>
          </thead>
          <tbody>
            {entries.map((e) => (
              <tr key={e.id}>
                {k.fields.map((f, i) => (
                  <td key={f.key} className={i === 0 ? '' : 'clip'} data-label={i === 0 ? undefined : f.label.replace(' (optional)', '')}>
                    {f.options ? f.options.find(([v]) => v === e.data[f.key])?.[1] : (e.data[f.key] ?? <span className="muted">—</span>)}
                  </td>
                ))}
                {editable && (
                  <td className="num nowrap">
                    <button className="ghost" onClick={() => setEditing({ id: e.id, form: Object.fromEntries(k.fields.map((f) => [f.key, e.data[f.key] ?? ''])) })}>
                      Edit
                    </button>
                    <button className="ghost danger" onClick={() => remove(e)}>
                      Delete
                    </button>
                  </td>
                )}
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {editing && (
        <EntryEditor
          kind={kind}
          id={editing.id}
          initial={editing.form}
          onClose={() => setEditing(null)}
          onSaved={() => {
            setEditing(null);
            lib.reload();
          }}
        />
      )}
    </Card>
  );
}

function EntryEditor({ kind, id, initial, onClose, onSaved }: { kind: Kind; id: string | null; initial: Record<string, string>; onClose: () => void; onSaved: () => void }) {
  const k = KINDS[kind];
  const [form, setForm] = useState(initial);
  const [touched, setTouched] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const errors = touched ? validate(kind, form) : {};
  const save = async () => {
    setTouched(true);
    if (Object.keys(validate(kind, form)).length) return;
    const data = Object.fromEntries(k.fields.map((f) => [f.key, f.optional && !form[f.key]!.trim() ? null : form[f.key]]));
    setError(null);
    setBusy(true);
    try {
      await (id ? api(`/api/team/library/${id}`, { method: 'PUT', json: { data } }) : api('/api/team/library', { json: { kind, data } }));
      onSaved();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal title={`${id ? 'Edit' : 'Add'} ${k.noun}`} onClose={onClose}>
      <div className="form">
        {k.fields.map((f) => {
          const set = (v: string) => setForm({ ...form, [f.key]: v });
          const err = errors[f.key];
          const props = { value: form[f.key] ?? '', 'aria-invalid': err ? true : undefined };
          return (
            <label key={f.key}>
              {f.label}
              {f.options ? (
                <select {...props} onChange={(e) => set(e.target.value)}>
                  {f.options.map(([v, l]) => (
                    <option key={v} value={v}>
                      {l}
                    </option>
                  ))}
                </select>
              ) : f.multiline ? (
                <textarea {...props} rows={5} onChange={(e) => set(e.target.value)} />
              ) : (
                <input {...props} onChange={(e) => set(e.target.value)} />
              )}
              {err && <span className="error small">{err}</span>}
            </label>
          );
        })}
      </div>
      <ErrorText error={error} />
      <div className="modal-actions">
        <button className="ghost" onClick={onClose}>
          Cancel
        </button>
        <button className="primary" disabled={busy} onClick={save}>
          Save
        </button>
      </div>
    </Modal>
  );
}
