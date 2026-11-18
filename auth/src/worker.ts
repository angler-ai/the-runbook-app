// Cloudflare Workers entry (cloud and own-Cloudflare editions): Better Auth on D1.
// Codes go out through the EMAIL `send_email` binding. The client IP comes from
// `cf-connecting-ip`. A misconfigured Worker cannot refuse to start, so it answers
// every request with 503 `auth.misconfigured` and logs the problems once.
import { drizzleAdapter } from '@better-auth/drizzle-adapter';
import { drizzle } from 'drizzle-orm/d1';
import { type App, createApp, createAuth } from './app';
import { ConfigError, loadConfig } from './config';
import { bindingTransport, cloudflareTransport, type EmailBinding, type EmailServiceBinding } from './email';
import { drizzleStore } from './limits';
import * as schema from './schema/sqlite';

export interface Env {
  DB: D1Database;
  EMAIL?: EmailBinding;
  /**
   * CF7: the Rust api Worker as a service binding. When present, the
   * can-register hook (D45) is called through it (RUNBOOK_AUTH_CAN_REGISTER_URL's
   * path is kept; a service binding ignores the host), so the two Workers talk
   * without a public hop.
   */
  API?: Fetcher;
  [key: string]: unknown;
}

// One app per isolate and environment object.
const apps = new WeakMap<object, App | 'misconfigured'>();

const misconfigured = () =>
  new Response(JSON.stringify({ code: 'auth.misconfigured', message: 'The sign-in service is misconfigured; see its logs.' }), {
    status: 503,
    headers: { 'content-type': 'application/json' },
  });

function build(env: Env): App | 'misconfigured' {
  let config;
  try {
    config = loadConfig(env, 'worker');
  } catch (error) {
    if (!(error instanceof ConfigError)) throw error;
    console.error(error.message);
    return 'misconfigured';
  }
  const db = drizzle(env.DB, { schema });
  const deps = {
    config,
    // D1 has no interactive transactions (Shelf: transaction: false).
    database: drizzleAdapter(db, { provider: 'sqlite', schema, transaction: false }),
    store: drizzleStore(db, schema),
    transport:
      config.email && env.EMAIL
        ? config.email.transport === 'cloudflare'
          ? cloudflareTransport(env.EMAIL as unknown as EmailServiceBinding, config.email.sender)
          : bindingTransport(env.EMAIL)
        : null,
    clientIpHeader: 'cf-connecting-ip',
    fetch: env.API
      ? (input: Request | string | URL, init?: RequestInit) => env.API!.fetch(new Request(input, init))
      : (input: Request | string | URL, init?: RequestInit) => fetch(input, init),
  };
  return createApp(deps, config.methods.length > 0 ? createAuth(deps) : null);
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    let app = apps.get(env);
    if (!app) {
      app = build(env);
      apps.set(env, app);
    }
    return app === 'misconfigured' ? misconfigured() : app.fetch(request);
  },
};
