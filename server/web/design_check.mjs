// Visual + alignment check for the "Quiet Glass" dashboard (docs/DESIGN.md).
//
// Usage: node web/design_check.mjs [outDir]
//   BASE_URL     server to test (default http://localhost:8793)
//   SESSION      value of the opstrellis_session cookie for an admin user
//   PW_DIR       directory with a throwaway `playwright` install (default /tmp/pwqa)
//   CHROMIUM     Chromium executable (default the preinstalled headless shell)
//
// For every page at 1440x900 and 390x844, light and dark, it asserts:
//   - the page title's left edge equals the left edge of every top-level card in the column (±1px)
//   - the header top equals the sidebar top, and the title is vertically centred with the brand row
//   - no horizontal page overflow
//   - buttons, inputs and selects are 32px tall (±1) on desktop
//   - visible text meets WCAG AA contrast against its effective background
// and saves a full-page screenshot of each.
import { createRequire } from 'node:module';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';

const PW_DIR = process.env.PW_DIR || '/tmp/pwqa';
const { chromium } = createRequire(join(PW_DIR, 'package.json'))('playwright');
const BASE = process.env.BASE_URL || 'http://localhost:8793';
const SESSION = process.env.SESSION || 'shotsession-qa-123';
const CHROMIUM = process.env.CHROMIUM || '/opt/pw-browsers/chromium_headless_shell-1194/chrome-linux/headless_shell';
const OUT = process.argv[2] || 'design-check-out';
mkdirSync(OUT, { recursive: true });

const PAGES = ['/login', '/', '/billing', '/tokens', '/team', '/settings', '/device', '/admin', '/admin/users', '/admin/models', '/admin/providers'];
const VIEWPORTS = { d: { width: 1440, height: 900 }, m: { width: 390, height: 844 } };
const failures = [];

async function deviceCode() {
  const res = await fetch(`${BASE}/api/device/start`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ device_name: 'Maya’s MacBook Pro' }) });
  return (await res.json()).user_code;
}

/** Runs in the page: returns a list of problems. */
function audit({ desktop, isAuth }) {
  const problems = [];
  const vis = (el) => {
    const r = el.getBoundingClientRect();
    const cs = getComputedStyle(el);
    return r.width > 0 && r.height > 0 && cs.visibility !== 'hidden' && cs.display !== 'none';
  };
  const px = (n) => Math.round(n * 10) / 10;
  const describe = (el) => `${el.tagName.toLowerCase()}${el.className && typeof el.className === 'string' ? '.' + el.className.trim().split(/\s+/).join('.') : ''}${el.textContent ? ` "${el.textContent.trim().slice(0, 30)}"` : ''}`;

  // horizontal overflow
  const sw = document.documentElement.scrollWidth;
  if (sw > innerWidth) problems.push(`horizontal overflow: scrollWidth ${sw} > ${innerWidth}`);

  if (isAuth) {
    const card = document.querySelector('.auth-card');
    const r = card.getBoundingClientRect();
    if (desktop && Math.abs(r.width - 360) > 1) problems.push(`auth card width ${px(r.width)} != 360`);
    const mid = r.left + r.width / 2;
    if (Math.abs(mid - innerWidth / 2) > 1) problems.push(`auth card not centred (${px(mid)} vs ${innerWidth / 2})`);
  } else {
    // title edge vs every top-level card / list in the column
    const h1 = document.querySelector('.page-head h1');
    const x = h1.getBoundingClientRect().left;
    const tops = [...document.querySelectorAll('.page-body > *')].filter(vis);
    for (const el of tops) {
      const targets = el.classList.contains('grid') ? [...el.children].filter(vis) : [el];
      // in a multi-column grid only the first column shares the title edge
      const firstColLeft = Math.min(...targets.map((t) => t.getBoundingClientRect().left));
      for (const t of targets) {
        const l = t.getBoundingClientRect().left;
        if (Math.abs(l - firstColLeft) < 1 && Math.abs(l - x) > 1) problems.push(`left edge ${px(l)} of ${describe(t)} != title ${px(x)}`);
      }
    }
    // the column's right edge: last column of each grid and full-width cards end together
    const rights = tops.map((el) => el.getBoundingClientRect().right).filter((r, i) => !tops[i].matches('.page-desc, .narrow, .error, .notice'));
    if (rights.length && Math.max(...rights) - Math.min(...rights) > 1) problems.push(`right edges differ: ${rights.map(px).join(', ')}`);
    // header / sidebar
    const head = document.querySelector('.page-head').getBoundingClientRect();
    const side = document.querySelector('.sidebar').getBoundingClientRect();
    if (desktop) {
      if (Math.abs(head.top - side.top) > 1) problems.push(`header top ${px(head.top)} != sidebar top ${px(side.top)}`);
      if (Math.abs(head.height - 64) > 1) problems.push(`header height ${px(head.height)} != 64`);
      if (Math.abs(side.width - 232) > 1) problems.push(`sidebar width ${px(side.width)} != 232`);
      if (Math.abs(side.height - innerHeight) > 1) problems.push(`sidebar not full height`);
      const brand = document.querySelector('.brand').getBoundingClientRect();
      const hr = h1.getBoundingClientRect();
      if (Math.abs(brand.top + brand.height / 2 - (hr.top + hr.height / 2)) > 1) problems.push('title not vertically centred with brand row');
      const actions = document.querySelector('.page-actions');
      if (actions) {
        const ar = actions.getBoundingClientRect();
        if (Math.abs(ar.top + ar.height / 2 - (hr.top + hr.height / 2)) > 1) problems.push('header actions not vertically centred with title');
      }
      for (const a of document.querySelectorAll('.nav-scroll nav a')) {
        if (Math.abs(a.getBoundingClientRect().height - 32) > 1) problems.push(`nav item ${describe(a)} not 32px`);
      }
    }
  }

  // controls 32px
  if (desktop) {
    for (const el of document.querySelectorAll('button, select, input:not([type=checkbox]):not([type=radio])')) {
      if (!vis(el)) continue;
      const h = el.getBoundingClientRect().height;
      if (Math.abs(h - 32) > 1) problems.push(`control ${describe(el)} is ${px(h)}px tall`);
    }
  }

  // AA contrast of text
  const parse = (c) => {
    const m = c.match(/rgba?\(([^)]+)\)/);
    if (!m) return null;
    const [r, g, b, a = 1] = m[1].split(/[ ,/]+/).filter(Boolean).map(Number);
    return [r, g, b, a];
  };
  const over = (top, bottom) => {
    const a = top[3];
    return [0, 1, 2].map((i) => top[i] * a + bottom[i] * (1 - a)).concat(1);
  };
  const bgOf = (el) => {
    const stack = [];
    for (let e = el; e; e = e.parentElement) {
      const c = parse(getComputedStyle(e).backgroundColor);
      if (c && c[3] > 0) {
        stack.push(c);
        if (c[3] >= 1) break;
      }
    }
    let col = parse(getComputedStyle(document.body).backgroundColor);
    for (let i = stack.length - 1; i >= 0; i--) col = over(stack[i], col);
    return col;
  };
  const lum = ([r, g, b]) => {
    const f = (v) => ((v /= 255) <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4);
    return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
  };
  const ratio = (a, b) => {
    const [l1, l2] = [lum(a), lum(b)].sort((x, y) => y - x);
    return (l1 + 0.05) / (l2 + 0.05);
  };
  const seen = new Set();
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    const el = n.parentElement;
    if (!el || seen.has(el) || !n.textContent.trim() || !vis(el)) continue;
    seen.add(el);
    if (el.closest('button:disabled, option, [aria-hidden=true], .tooltip')) continue;
    const cs = getComputedStyle(el);
    let fg = parse(cs.color);
    const bg = bgOf(el);
    fg = over(fg, bg);
    let opacity = 1;
    for (let e = el; e; e = e.parentElement) opacity *= Number(getComputedStyle(e).opacity);
    if (opacity < 1) fg = over([...fg.slice(0, 3), opacity], bg);
    const size = parseFloat(cs.fontSize);
    const large = size >= 24 || (size >= 18.66 && Number(cs.fontWeight) >= 700);
    const need = large ? 3 : 4.5;
    const r = ratio(fg, bg);
    if (r < need) problems.push(`contrast ${r.toFixed(2)} < ${need} for ${describe(el)}`);
  }
  return problems;
}

const browser = await chromium.launch({ executablePath: CHROMIUM });
try {
  for (const scheme of ['light', 'dark']) {
    for (const [vk, viewport] of Object.entries(VIEWPORTS)) {
      for (const path of PAGES) {
        const isAuth = path === '/login';
        const ctx = await browser.newContext({ viewport, colorScheme: scheme, deviceScaleFactor: vk === 'm' ? 2 : 1 });
        if (!isAuth) await ctx.addCookies([{ name: 'opstrellis_session', value: SESSION, url: BASE }]);
        const page = await ctx.newPage();
        const url = path === '/device' ? `/device?code=${await deviceCode()}` : path;
        await page.goto(BASE + url, { waitUntil: 'networkidle' });
        await page.waitForSelector(isAuth ? '.auth-card' : '.page-head h1');
        await page.waitForFunction(() => !document.querySelector('.loading'), null, { timeout: 5000 }).catch(() => {});
        await page.evaluate(() => document.fonts.ready);
        const problems = await page.evaluate(audit, { desktop: vk === 'd', isAuth });
        const name = `${path === '/' ? 'overview' : path.slice(1).replace(/\//g, '-')}-${vk}-${scheme}`;
        await page.screenshot({ path: join(OUT, `${name}.png`), fullPage: true });
        for (const p of problems) failures.push(`${name}: ${p}`);
        console.log(`${problems.length ? 'FAIL' : 'ok  '} ${name}${problems.length ? ` (${problems.length})` : ''}`);
        await ctx.close();
      }
    }
  }
} finally {
  await browser.close();
}
if (failures.length) {
  console.log(`\n${failures.length} problem(s):\n` + failures.join('\n'));
  process.exit(1);
}
console.log('\nAll checks passed.');
