import '@fontsource-variable/inter';
import { StrictMode, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { api, HttpError, type Me } from './api';
import { AdminModels, AdminOverview, AdminProviders, AdminUsers } from './pages/admin';
import { AuthPage } from './pages/auth';
import { Team } from './pages/team';
import { Billing, Device, Overview, Settings, Tokens } from './pages/user';
import { Link, navigate, useLocation } from './router';
import { Icon, Logo, Page, type IconName } from './ui';
import './styles.css';

type NavItem = readonly [href: string, label: string, icon: IconName];
const USER_NAV: readonly NavItem[] = [
  ['/', 'Overview', 'overview'],
  ['/billing', 'Plan & billing', 'billing'],
  ['/tokens', 'Devices & tokens', 'devices'],
  ['/team', 'Team', 'team'],
  ['/settings', 'Settings', 'settings'],
];
const ADMIN_NAV: readonly NavItem[] = [
  ['/admin', 'Revenue & cost', 'revenue'],
  ['/admin/users', 'Users', 'users'],
  ['/admin/models', 'Model catalog', 'models'],
  ['/admin/providers', 'Provider keys', 'key'],
];

/** On phones the nav is a horizontal strip: keep the active item in view without scrolling the page. */
function useActiveInView(dep: unknown) {
  const navRef = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const nav = navRef.current;
    const a = nav?.querySelector<HTMLElement>('a.active');
    if (!nav || !a || nav.scrollWidth <= nav.clientWidth) return;
    nav.scrollLeft = a.offsetLeft - (nav.clientWidth - a.offsetWidth) / 2;
  }, [dep]);
  return navRef;
}

function App() {
  const { path, search } = useLocation();
  const [me, setMe] = useState<Me | null | undefined>(undefined);
  const navRef = useActiveInView(`${path}|${me?.user.role}`);
  const refresh = () =>
    api<Me>('/api/me').then(setMe, (e) => {
      if (e instanceof HttpError && (e.status === 401 || e.status === 403)) setMe(null);
      else throw e;
    });
  useEffect(() => {
    refresh();
  }, []);

  if (me === undefined) return null;
  if (!me) {
    const next = path === '/login' || path === '/signup' ? new URLSearchParams(search).get('next') || '/' : path + search;
    return <AuthPage mode={path === '/signup' ? 'signup' : 'login'} next={next} onAuthed={refresh} />;
  }
  if (path === '/login' || path === '/signup') {
    navigate(new URLSearchParams(search).get('next') || '/');
    return null;
  }

  const isAdmin = me.user.role === 'admin';
  const page = (() => {
    switch (path) {
      case '/':
        return <Overview me={me} />;
      case '/billing':
        return <Billing me={me} />;
      case '/tokens':
        return <Tokens />;
      case '/team':
        return <Team meId={me.user.id} />;
      case '/settings':
        return <Settings me={me} onSaved={refresh} />;
      case '/device':
        return <Device code={new URLSearchParams(search).get('code') ?? ''} />;
    }
    if (isAdmin) {
      if (path === '/admin') return <AdminOverview />;
      if (path === '/admin/users') return <AdminUsers />;
      if (path === '/admin/models') return <AdminModels />;
      if (path === '/admin/providers') return <AdminProviders />;
    }
    return (
      <Page title="Not found">
        <p className="muted">This page does not exist.</p>
      </Page>
    );
  })();

  const logout = async () => {
    await api('/api/auth/logout', { method: 'POST' });
    setMe(null);
    navigate('/login');
  };

  const nav = (items: readonly NavItem[]) =>
    items.map(([href, label, icon]) => (
      <Link key={href} href={href} className={path === href ? 'active' : ''} aria-current={path === href ? 'page' : undefined}>
        <Icon name={icon} size={16} />
        <span>{label}</span>
      </Link>
    ));
  const who = me.user.name || me.user.email;
  const left = me.usage.remaining_minutes;

  return (
    <div className="shell">
      <aside className="sidebar">
        <div className="brand">
          <Logo />
          <span className="brand-name">
            OpsTrellis <span className="brand-sub">Cloud</span>
          </span>
        </div>
        <div className="nav-scroll" ref={navRef}>
          <div className="nav-label">Workspace</div>
          <nav aria-label="Workspace">{nav(USER_NAV)}</nav>
          {isAdmin && (
            <>
              <div className="nav-label">Admin</div>
              <nav aria-label="Admin">{nav(ADMIN_NAV)}</nav>
            </>
          )}
        </div>
        <div className="sidebar-foot">
          <div className={`status-chip ${left <= 0 ? 'out' : ''}`}>
            <span className="dot" aria-hidden />
            {left > 0 ? `${left.toLocaleString(undefined, { maximumFractionDigits: 1 })} cloud min left` : 'Cloud minutes used up'}
          </div>
          <div className="who">
            <span className="avatar" aria-hidden>
              {who.trim().charAt(0).toUpperCase()}
            </span>
            <div className="who-text">
              <strong>{who}</strong>
              <span>
                {me.plan === 'pro' ? 'Pro' : 'Free'} plan{isAdmin ? ' · admin' : ''}
              </span>
            </div>
            <button className="icon-btn" onClick={logout} aria-label="Sign out" title="Sign out">
              <Icon name="logout" size={16} />
            </button>
          </div>
        </div>
      </aside>
      <main>{page}</main>
    </div>
  );
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
