import { useState, type FormEvent } from 'react';
import { api } from '../api';
import { Link, navigate } from '../router';
import { ErrorText, Logo, useAsync } from '../ui';

export function AuthPage({ mode, next, onAuthed }: { mode: 'login' | 'signup'; next: string; onAuthed: () => void }) {
  const [form, setForm] = useState({ name: '', email: '', password: '' });
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const signup = useAsync(() => api<{ open: boolean }>('/api/auth/signup'));
  const signupOpen = signup.data?.open ?? true;
  const closed = mode === 'signup' && signup.data?.open === false;
  const qs = next && next !== '/' ? `?next=${encodeURIComponent(next)}` : '';

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api(`/api/auth/${mode}`, { json: mode === 'login' ? { email: form.email, password: form.password } : form });
      navigate(next);
      onAuthed();
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };
  const set = (k: keyof typeof form) => (e: { target: { value: string } }) => setForm({ ...form, [k]: e.target.value });

  return (
    <main className="auth">
      <form className="card auth-card" onSubmit={submit}>
        <div className="auth-head">
          <Logo size={32} />
          <h1>{mode === 'login' ? 'Sign in to OpsTrellis Cloud' : 'Create your OpsTrellis Cloud account'}</h1>
          <p className="muted">{next.startsWith('/device') ? 'Sign in to connect the OpsTrellis desktop app.' : 'Voice dictation, everywhere you type.'}</p>
        </div>
        {closed ? (
          <p className="notice">Signup is closed on this server. Ask an administrator for an account.</p>
        ) : (
          <>
            {mode === 'signup' && (
              <label>
                Name
                <input value={form.name} onChange={set('name')} autoComplete="name" />
              </label>
            )}
            <label>
              Email
              <input type="email" required value={form.email} onChange={set('email')} autoComplete="email" placeholder="you@company.com" />
            </label>
            <label>
              Password
              <input type="password" required minLength={mode === 'signup' ? 8 : 1} value={form.password} onChange={set('password')} autoComplete={mode === 'login' ? 'current-password' : 'new-password'} placeholder={mode === 'signup' ? 'At least 8 characters' : undefined} />
            </label>
            <ErrorText error={error} />
            <button className="primary block" disabled={busy}>
              {mode === 'login' ? 'Sign in' : 'Create account'}
            </button>
          </>
        )}
        {mode === 'login' ? (
          signupOpen && (
            <p className="auth-switch">
              New here? <Link href={`/signup${qs}`}>Create an account</Link>
            </p>
          )
        ) : (
          <p className="auth-switch">
            Already have an account? <Link href={`/login${qs}`}>Sign in</Link>
          </p>
        )}
      </form>
    </main>
  );
}
