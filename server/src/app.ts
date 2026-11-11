import { serveStatic } from '@hono/node-server/serve-static';
import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { Hono } from 'hono';
import { secureHeaders } from 'hono/secure-headers';
import type { AppEnv } from './auth.js';
import { config } from './config.js';
import { ApiError, apiError, jsonBodyLimit } from './lib/http.js';
import { admin } from './routes/admin.js';
import { api } from './routes/api.js';
import { authRoutes } from './routes/auth.js';
import { gateway } from './routes/gateway.js';
import { team } from './routes/team.js';

export function createApp() {
  const app = new Hono<AppEnv>();
  app.use('*', secureHeaders({ crossOriginResourcePolicy: false }));

  app.get('/healthz', (c) => c.json({ ok: true }));
  // JSON bodies under /api are small; usage uploads and Stripe webhooks set their own 1 MB limit.
  const smallBody = jsonBodyLimit(64 * 1024);
  const ownLimit = new Set(['/api/usage', '/api/stripe/webhook']);
  app.use('/api/*', (c, next) => (ownLimit.has(c.req.path) ? next() : smallBody(c, next)));
  app.route('/v1', gateway);
  app.route('/api', authRoutes);
  app.route('/api/admin', admin);
  app.route('/api', api);
  app.route('/api', team);
  app.all('/api/*', (c) => apiError(c, 404, 'not_found'));
  app.all('/v1/*', (c) => apiError(c, 404, 'not_found'));

  // Dashboard SPA: static assets, then index.html for client-side routes.
  const webDir = path.resolve(config.webDir);
  const indexFile = path.join(webDir, 'index.html');
  app.get('*', serveStatic({ root: path.relative(process.cwd(), webDir) || '.' }));
  app.get('*', (c) => {
    if (!existsSync(indexFile)) return c.text('Dashboard not built. Run `npm run build`.', 404);
    return c.html(readFileSync(indexFile, 'utf8'));
  });

  app.onError((err, c) => {
    if (err instanceof ApiError) return apiError(c, err.status, err.code, err.message === err.code ? undefined : err.message);
    console.error(err);
    return apiError(c, 500, 'internal_error', 'Something went wrong');
  });
  return app;
}
