// Node/Bun entry (Docker, self-hosted): Better Auth on Postgres, in the `auth`
// schema with its own migrations (D43). `runbook-server serve` spawns this as a
// child on loopback, passes the D48 variables plus the per-boot internal secret,
// and proxies /api/auth/* to it (README "Contract for AUTH2").
//
// Startup: applies the migrations under a Postgres advisory lock, listens on
// 127.0.0.1:$RUNBOOK_AUTH_PORT (0 picks a free port) and prints one JSON line to
// stdout: {"event":"ready","port":N,"methods":[...]}. A configuration error
// prints every problem to stderr and exits with status 78 (EX_CONFIG).
import { readFileSync } from 'node:fs';
import { appendFile } from 'node:fs/promises';
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import { fileURLToPath } from 'node:url';
import { drizzleAdapter } from '@better-auth/drizzle-adapter';
import { drizzle } from 'drizzle-orm/postgres-js';
import { migrate } from 'drizzle-orm/postgres-js/migrator';
import nodemailer from 'nodemailer';
import postgres from 'postgres';
import { type AppDeps, createApp, createAuth } from './app';
import { ConfigError, loadConfig, type AuthConfig } from './config';
import { databaseSettings } from './database';
import { captureTransport, type MailTransport, smtpTransport } from './email';
import { drizzleStore } from './limits';
import { deliverMfaNotifications } from './mfa';
import * as schema from './schema/pg';

export { databaseSettings } from './database';
export const EX_CONFIG = 78;
const MAX_REQUEST_BYTES = 64 * 1024;
const MIGRATION_LOCK = 'runbook-auth-migrations';

export interface RunningServer {
  port: number;
  url: string;
  methods: AuthConfig['methods'];
  close(): Promise<void>;
}

function serverSettings(env: Record<string, string | undefined>) {
  const problems: string[] = [];
  const databaseUrl = env.DATABASE_URL?.trim();
  if (!databaseUrl) problems.push('DATABASE_URL is required (the same Postgres database as runbook-server; tables live in the `auth` schema)');
  const portRaw = env.RUNBOOK_AUTH_PORT?.trim() || '0';
  const port = Number(portRaw);
  if (!Number.isInteger(port) || port < 0 || port > 65535) problems.push(`RUNBOOK_AUTH_PORT must be a port number, got ${JSON.stringify(portRaw)}`);
  const migrationsFolder = env.RUNBOOK_AUTH_MIGRATIONS_DIR?.trim() || fileURLToPath(new URL('../drizzle/pg', import.meta.url));
  // HOST1: sslmode, sslrootcert, a Cloud SQL socket and the pool size, read
  // as runbook-server reads them.
  const database = databaseUrl ? databaseSettings(databaseUrl, env, (path) => readFileSync(path, 'utf8'), problems) : null;
  return { problems, database, port, migrationsFolder };
}

function transportFor(config: AuthConfig): MailTransport | null {
  const email = config.email;
  if (!email) return null;
  if (email.transport === 'smtp') return smtpTransport((url) => nodemailer.createTransport(url), email.smtpUrl);
  if (email.transport === 'capture') return captureTransport(email.captureFile, (file, line) => appendFile(file, line, { mode: 0o600 }));
  return null;
}

/** Buffers a Node request into a Fetch Request (auth bodies are small). */
async function toRequest(req: IncomingMessage): Promise<Request | null> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of req as AsyncIterable<Buffer>) {
    size += chunk.length;
    if (size > MAX_REQUEST_BYTES) return null;
    chunks.push(chunk);
  }
  const headers = new Headers();
  for (const [name, value] of Object.entries(req.headers)) {
    if (value === undefined) continue;
    for (const v of Array.isArray(value) ? value : [value]) headers.append(name, v);
  }
  const method = req.method ?? 'GET';
  const body = method === 'GET' || method === 'HEAD' || size === 0 ? undefined : Buffer.concat(chunks);
  return new Request(`http://${req.headers.host ?? '127.0.0.1'}${req.url ?? '/'}`, { method, headers, body });
}

async function writeResponse(res: ServerResponse, response: Response): Promise<void> {
  const headers: Record<string, string | string[]> = {};
  response.headers.forEach((value, name) => {
    if (name !== 'set-cookie') headers[name] = value;
  });
  const cookies = response.headers.getSetCookie();
  if (cookies.length > 0) headers['set-cookie'] = cookies;
  const body = Buffer.from(await response.arrayBuffer());
  res.writeHead(response.status, headers);
  res.end(body);
}

/**
 * Starts the service. Throws ConfigError on a bad configuration. Never binds
 * anything but 127.0.0.1: the Rust host is the only client.
 */
export async function startServer(env: Record<string, string | undefined> = process.env): Promise<RunningServer> {
  const settings = serverSettings(env);
  let config: AuthConfig | null = null;
  try {
    config = loadConfig(env, 'server');
  } catch (error) {
    if (!(error instanceof ConfigError)) throw error;
    throw new ConfigError([...error.problems, ...settings.problems]);
  }
  if (settings.problems.length > 0) throw new ConfigError(settings.problems);

  const sql = postgres(settings.database!.url, { ...settings.database!.options, onnotice: () => {} });
  const db = drizzle(sql, { schema });
  try {
    const lock = await sql.reserve();
    try {
      await lock`select pg_advisory_lock(hashtext(${MIGRATION_LOCK}))`;
      await migrate(db, { migrationsFolder: settings.migrationsFolder, migrationsSchema: 'auth', migrationsTable: '__drizzle_migrations' });
    } finally {
      await lock`select pg_advisory_unlock(hashtext(${MIGRATION_LOCK}))`.catch(() => {});
      lock.release();
    }
  } catch (error) {
    await sql.end({ timeout: 1 });
    throw error;
  }

  const deps: AppDeps = {
    config,
    database: drizzleAdapter(db, { provider: 'pg', schema }),
    store: drizzleStore(db, schema),
    transport: transportFor(config),
    // Rust's proxy sets this to the client address: the TCP peer, or, when the
    // peer is in RUNBOOK_TRUSTED_PROXIES, the right-most untrusted
    // X-Forwarded-For / Forwarded hop. It drops every client-sent address header
    // (this one included), so nothing else here reads one.
    clientIpHeader: 'x-runbook-client-ip',
    fetch: (input, init) => fetch(input, init),
  };
  const app = createApp(deps, config.methods.length > 0 ? createAuth(deps) : null);

  const server: Server = createServer((req, res) => {
    void (async () => {
      try {
        const request = await toRequest(req);
        const response = request
          ? await app.fetch(request)
          : new Response(JSON.stringify({ code: 'auth.too_large', message: 'Request body too large.' }), { status: 413, headers: { 'content-type': 'application/json' } });
        await writeResponse(res, response);
      } catch {
        if (!res.headersSent) res.writeHead(500, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ code: 'auth.internal_error', message: 'Internal error.' }));
      }
    })();
  });
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(settings.port, '127.0.0.1', () => resolve());
  });
  const notifications = setInterval(() => { void deliverMfaNotifications(deps); }, 60_000).unref();
  const address = server.address();
  const port = typeof address === 'object' && address ? address.port : settings.port;
  return {
    port,
    url: `http://127.0.0.1:${port}`,
    methods: config.methods,
    // HOST1: bounded, so SIGTERM from runbook-server (which waits about a
    // second and a half) ends with the pool closed: idle keep-alive
    // connections close at once, busy ones get a moment to finish.
    async close() {
      clearInterval(notifications);
      const closed = new Promise<void>((resolve) => server.close(() => resolve()));
      server.closeIdleConnections?.();
      await Promise.race([closed, new Promise((resolve) => setTimeout(resolve, 1000).unref())]);
      server.closeAllConnections?.();
      await sql.end({ timeout: 1 });
    },
  };
}

/** Called by the native server's operator command. No listener is opened. */
async function operatorReset(email: string): Promise<void> {
  const settings = serverSettings(process.env);
  if (settings.problems.length || !settings.database) throw new ConfigError(settings.problems);
  const sql = postgres(settings.database.url, { ...settings.database.options, onnotice: () => {} });
  try {
    let config: AuthConfig | null = null;
    try {
      config = loadConfig({ ...process.env, RUNBOOK_AUTH_INTERNAL_SECRET: 'operator-reset-no-listener-000000000', RUNBOOK_AUTH_CAN_REGISTER_URL: 'http://127.0.0.1:1/internal/can-register' }, 'server');
    } catch (error) {
      if (!(error instanceof ConfigError)) throw error;
    }
    const store = drizzleStore(drizzle(sql, { schema }), schema);
    const found = await store.resetMfa(email, null, 'operator');
    if (found.length === 0) throw new Error('No auth account with that email address.');
    if (config) await deliverMfaNotifications({ config, store, transport: transportFor(config) }, found[0].id);
    process.stdout.write('MFA reset recorded. Existing sessions and OAuth tokens were revoked.\n');
  } finally {
    await sql.end({ timeout: 1 });
  }
}

async function main(): Promise<void> {
  if (process.argv[2] === 'mfa-reset') {
    try {
      const email = process.argv[3]?.trim().toLowerCase();
      if (!email || process.argv.length !== 4 || !/^[^\s@]+@[^\s@]+$/.test(email)) throw new Error('A valid email address is required.');
      await operatorReset(email);
    } catch (error) {
      process.stderr.write(error instanceof ConfigError ? `${error.message}\n` : 'MFA reset could not be completed. Check the account and auth database.\n');
      process.exitCode = 1;
    }
    return;
  }
  let running: RunningServer;
  try {
    running = await startServer(process.env);
  } catch (error) {
    if (error instanceof ConfigError) {
      process.stderr.write(`${error.message}\n`);
      process.exit(EX_CONFIG);
    }
    process.stderr.write(`runbook-auth: failed to start: ${error instanceof Error ? error.message : String(error)}\n`);
    process.exit(1);
  }
  process.stdout.write(`${JSON.stringify({ event: 'ready', port: running.port, methods: running.methods })}\n`);
  if (running.methods.length === 0) process.stderr.write('runbook-auth: no sign-in method is configured; /api/auth answers 503 until one is.\n');
  const stop = () => {
    void running.close().then(() => process.exit(0));
  };
  process.once('SIGTERM', stop);
  process.once('SIGINT', stop);
}

const entry = process.argv[1];
if (entry && fileURLToPath(import.meta.url) === entry) void main();
