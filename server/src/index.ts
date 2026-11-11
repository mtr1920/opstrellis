import { serve } from '@hono/node-server';
import { createApp } from './app.js';
import { purgeExpired } from './auth.js';
import { config, configError, configWarnings } from './config.js';
import { bootstrapAdmin } from './db/seed.js';
import { bootstrapWarning } from './routes/auth.js';
import { purgeTombstones } from './routes/team.js';
import { reportOverage } from './stripe.js';

const problem = configError();
if (problem) {
  console.error(`Refusing to start: ${problem}`);
  process.exit(1);
}
for (const w of configWarnings()) console.warn(`WARNING: ${w}`);

// Before listening, so nothing can sign in to (or mint a token for) an account while it is being promoted.
try {
  const admin = await bootstrapAdmin();
  if (admin) console.log(`admin ${config.adminEmail} ${admin}`);
} catch (e) {
  console.error(`Refusing to start: ${(e as Error).message}`);
  process.exit(1);
}

const server = serve({ fetch: createApp().fetch, port: config.port }, (info) => {
  console.log(`OpsTrellis Cloud listening on http://localhost:${info.port} (public URL ${config.publicUrl})`);
});

bootstrapWarning().then(
  (w) => w && console.warn(`\n${'!'.repeat(80)}\n${w}\n${'!'.repeat(80)}\n`),
  (e) => console.error(e),
);

// Retry any overage minutes that could not be sent to Stripe inline.
setInterval(() => reportOverage().catch((e) => console.error(e)), 5 * 60_000).unref();

setInterval(() => purgeExpired().catch((e) => console.error(e)), 60 * 60_000).unref();
setInterval(() => purgeTombstones().catch((e) => console.error(e)), 6 * 60 * 60_000).unref();

for (const sig of ['SIGINT', 'SIGTERM']) {
  process.on(sig, () => server.close(() => process.exit(0)));
}
