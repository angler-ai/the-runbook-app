// CF1 harness (the Shelf pattern): run the built Rust Worker under workerd
// through Miniflare, with bindings read from the LOCAL-ONLY wrangler.jsonc.
// Nothing here reaches a Cloudflare account: D1, R2 and the Durable Object
// are Miniflare's local simulators in a temporary directory.
import assert from 'node:assert/strict';
import { Miniflare, convertV4MiniflareOptions } from 'miniflare';
import { unstable_getMiniflareWorkerOptions } from 'wrangler';
import { existsSync } from 'node:fs';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { crc32, deflateRawSync, inflateRawSync } from 'node:zlib';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

process.env.WRANGLER_SEND_METRICS = 'false';

export const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
// The deploy build (no test seams; what the size gate measures) and the
// test build (`--features test-seams`, gate fix B1) the workerd tests run.
export const buildDir = join(root, 'build');
export const testBuildDir = join(root, 'build-test');
export const wasmPath = join(buildDir, 'index_bg.wasm');
export const scriptPath = join(buildDir, 'index.js');

export function assertBuilt(dir = buildDir) {
  if (!existsSync(join(dir, 'index_bg.wasm')) || !existsSync(join(dir, 'index.js'))) {
    throw new Error(`${dir} is missing: run \`npm run build\` (or \`npm test\`) first`);
  }
}

/** The CF2 test Worker that runs the store conformance cases. */
export const CONFORMANCE = {
  config: 'wrangler.conformance.jsonc',
  dir: join(root, 'build-conformance'),
  name: 'runbook-conformance-local-cf2',
};
const API = { config: 'wrangler.jsonc', dir: testBuildDir, name: 'runbook-api-local-cf1' };
/** The deploy build of the api Worker (B1: no test seams). */
export const DEPLOY = { config: 'wrangler.jsonc', dir: buildDir, name: 'runbook-api-local-deploy' };

/** Start one local workerd with the api Worker (or `worker`, e.g. CONFORMANCE). */
/** `d1`: more local D1 bindings beside the config's (CF11's shards). */
export async function startWorker({ faults = true, worker = API, vars = {}, d1 = {}, without = [] } = {}) {
  assertBuilt(worker.dir);
  const temp = await mkdtemp(join(tmpdir(), 'runbook-cf-'));
  // Bindings, the SQLite Durable Object class and the compatibility date
  // come from wrangler.jsonc. Miniflare 5 takes an explicit module list
  // (its v4 converter refuses `modulesRules`): the worker-build entry and
  // the wasm it imports as `./index_bg.wasm`.
  const { workerOptions } = unstable_getMiniflareWorkerOptions(join(root, worker.config));
  const { modulesRules: _rules, rootPath: _root, ...options } = workerOptions;
  // `without`: Durable Object bindings to leave out (a misconfigured deploy).
  const durableObjects = Object.fromEntries(
    Object.entries(options.durableObjects ?? {}).filter(([name]) => !without.includes(name)),
  );
  const mf = new Miniflare(
    convertV4MiniflareOptions({
      ...options,
      durableObjects,
      d1Databases: { ...(options.d1Databases ?? {}), ...d1 },
      name: worker.name,
      modulesRoot: worker.dir,
      modules: [
        { type: 'ESModule', path: join(worker.dir, 'index.js') },
        { type: 'CompiledWasm', path: join(worker.dir, 'index_bg.wasm') },
      ],
      // A short keep-alive so idle streams show one within a test (CF4).
      // Local test hooks: the object's fault injection and spike routes, and
      // private (loopback) webhook targets (CF4).
      bindings: {
        ...(faults
          ? { CF1_FAULT_INJECTION: 'on', WEBHOOKS_ALLOW_PRIVATE: 'on' }
          : {}),
        ...(worker === DEPLOY ? {} : { SSE_KEEPALIVE_MS: '400' }),
        ...vars,
      },
      defaultPersistRoot: temp,
    }),
  );
  await mf.ready;
  const fetch = (path, init) => mf.dispatchFetch(`https://runbook.test${path}`, init);
  return {
    mf,
    fetch,
    async json(path, init) {
      const response = await fetch(path, init);
      return { status: response.status, body: await response.json() };
    },
    /** One ExecutionDO's raw response, bypassing the api Worker (test hooks). */
    async objectRaw(id, path, init) {
      const namespace = await mf.getDurableObjectNamespace('EXECUTIONS');
      return namespace.get(namespace.idFromName(id)).fetch(`https://do/${id}${path}`, init);
    },
    /** Call one ExecutionDO directly, bypassing the api Worker (test hooks). */
    async object(id, path, init) {
      const namespace = await mf.getDurableObjectNamespace('EXECUTIONS');
      const response = await namespace.get(namespace.idFromName(id)).fetch(`https://do/${id}${path}`, init);
      return { status: response.status, body: await response.json() };
    },
    async dispose() {
      await mf.dispose();
      await rm(temp, { recursive: true, force: true });
    },
  };
}

let counter = 0;
/** A fresh, valid execution ID per call (one Durable Object each). */
export function executionId() {
  counter += 1;
  const tail = `${process.pid.toString(32)}${counter.toString(32)}`.toUpperCase().replace(/[ILOU]/g, 'Z');
  return `exe_0${tail.padStart(25, '0')}`.slice(0, 30);
}

export const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

/**
 * Read an SSE response frame by frame. `next(ms)` resolves to the next
 * frame's text (without the blank line), or null when the stream ended.
 */
export function frames(response) {
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = '';
  let ended = false;
  return {
    async next(ms = 5000) {
      const deadline = Date.now() + ms;
      for (;;) {
        const end = buffer.indexOf('\n\n');
        if (end >= 0) {
          const frame = buffer.slice(0, end);
          buffer = buffer.slice(end + 2);
          return frame;
        }
        if (ended) return null;
        const left = deadline - Date.now();
        if (left <= 0) throw new Error(`no SSE frame within ${ms} ms`);
        const read = await Promise.race([
          reader.read(),
          sleep(left).then(() => ({ timeout: true })),
        ]);
        if (read.timeout) continue;
        if (read.done) ended = true;
        else buffer += decoder.decode(read.value, { stream: true });
      }
    },
    async cancel() {
      await reader.cancel().catch(() => {});
    },
  };
}

/** The parts of one SSE event frame. */
export function parseFrame(text) {
  const out = {};
  for (const line of text.split('\n')) {
    const at = line.indexOf(': ');
    if (line.startsWith(':')) out.comment = line.slice(1).trim();
    else if (at > 0) out[line.slice(0, at)] = line.slice(at + 2);
  }
  if (out.data) out.json = JSON.parse(out.data);
  return out;
}

/**
 * CF7: the api Worker with the sign-in service: the `auth/` Worker
 * (auth/dist/worker.mjs, Better Auth) on the same D1, bound as `AUTH`, with
 * the api Worker bound back as its `API` (can-register) and a recording
 * stand-in for its `EMAIL` binding. Returns startWorker's helpers plus
 * `mail()` (the codes sent) and `migrateAuth()`.
 */
export const AUTH_INTERNAL_SECRET = 'cf7-internal-secret-0123456789abcdef0123456789';
export const AUTH_SECRET = 'cf7-auth-secret-which-is-at-least-32-bytes-long';
const MAIL_WORKER = `
import { WorkerEntrypoint } from 'cloudflare:workers';
const messages = [];
export class Mail extends WorkerEntrypoint {
  async send(message) {
    messages.push({ to: message.to, code: /^(\\d{6}) /.exec(message.subject)?.[1] ?? null });
    return { messageId: 'test' };
  }
}
export default { fetch() { return Response.json(messages); } };`;

/**
 * `port`: also listen on 127.0.0.1:<port> for a real browser (MCP2's
 * Playwright test); `origin` must then be http://127.0.0.1:<port>.
 */
export async function startSignInStack({ vars = {}, origin = 'https://runbook.test', port } = {}) {
  assertBuilt(testBuildDir);
  const authScript = join(root, '..', 'auth', 'dist', 'worker.mjs');
  if (!existsSync(authScript)) {
    throw new Error(`${authScript} is missing: run \`npm run build\` in auth/ first`);
  }
  const { readFile, readdir } = await import('node:fs/promises');
  const temp = await mkdtemp(join(tmpdir(), 'runbook-cf-auth-'));
  const { workerOptions } = unstable_getMiniflareWorkerOptions(join(root, API.config));
  const { modulesRules: _rules, rootPath: _root, ...options } = workerOptions;
  const mf = new Miniflare(
    convertV4MiniflareOptions({
      defaultPersistRoot: temp,
      ...(port ? { host: '127.0.0.1', port } : {}),
      workers: [
        {
          ...options,
          name: API.name,
          modulesRoot: testBuildDir,
          modules: [
            { type: 'ESModule', path: join(testBuildDir, 'index.js') },
            { type: 'CompiledWasm', path: join(testBuildDir, 'index_bg.wasm') },
          ],
          serviceBindings: { ...(options.serviceBindings ?? {}), AUTH: 'runbook-auth' },
          bindings: {
            ORIGIN: origin,
            RUNBOOK_AUTH_INTERNAL_SECRET: AUTH_INTERNAL_SECRET,
            SSE_KEEPALIVE_MS: '400',
            ...vars,
          },
        },
        {
          name: 'runbook-auth',
          modules: true,
          script: await readFile(authScript, 'utf8'),
          compatibilityDate: '2026-09-01',
          compatibilityFlags: ['nodejs_compat'],
          d1Databases: options.d1Databases,
          stripCfConnectingIp: false,
          serviceBindings: { API: API.name, EMAIL: { name: 'mail', entrypoint: 'Mail' } },
          bindings: {
            RUNBOOK_AUTH_INTERNAL_SECRET: AUTH_INTERNAL_SECRET,
            RUNBOOK_AUTH_SECRET: AUTH_SECRET,
            RUNBOOK_ORIGIN: origin,
            RUNBOOK_AUTH_METHODS: 'email',
            RUNBOOK_AUTH_CAN_REGISTER_URL: 'https://api.internal/internal/can-register',
            RUNBOOK_SMTP_FROM: 'The Runbook <login@example.test>',
          },
        },
        { name: 'mail', modules: true, script: MAIL_WORKER, compatibilityDate: '2026-09-01' },
      ],
    }),
  );
  await mf.ready;
  // The auth tables (auth/drizzle/sqlite) beside the api's in the one D1.
  const db = await mf.getD1Database('DB', API.name);
  const dir = join(root, '..', 'auth', 'drizzle', 'sqlite');
  for (const file of (await readdir(dir)).filter((f) => f.endsWith('.sql')).sort()) {
    const statements = (await readFile(join(dir, file), 'utf8'))
      .split('--> statement-breakpoint')
      .map((s) => s.trim())
      .filter(Boolean);
    await db.batch(statements.map((s) => db.prepare(s)));
  }
  const fetch = (path, init) => mf.dispatchFetch(`${origin}${path}`, { redirect: 'manual', ...init });
  const mailWorker = await mf.getWorker('mail');
  return {
    mf,
    db,
    origin,
    fetch,
    async mail() {
      return (await mailWorker.fetch('http://mail/')).json();
    },
    async dispose() {
      await mf.dispose();
      await rm(temp, { recursive: true, force: true });
    },
  };
}

/** The files of a ZIP (stored or deflated entries), by name. */
/** CF12: a ZIP of `files` (a Map of name to bytes), stored, or deflated with `deflate`. */
export function zip(files, { deflate = false } = {}) {
  const locals = [];
  const central = [];
  let offset = 0;
  for (const [name, content] of files) {
    const nameBytes = Buffer.from(name, 'utf8');
    const raw = Buffer.from(content);
    const crc = crc32(raw);
    const data = deflate ? deflateRawSync(raw) : raw;
    const method = deflate ? 8 : 0;
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(method, 8);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(data.length, 18);
    local.writeUInt32LE(raw.length, 22);
    local.writeUInt16LE(nameBytes.length, 26);
    const entry = Buffer.alloc(46);
    entry.writeUInt32LE(0x02014b50, 0);
    entry.writeUInt16LE(20, 4);
    entry.writeUInt16LE(20, 6);
    entry.writeUInt16LE(method, 10);
    entry.writeUInt32LE(crc, 16);
    entry.writeUInt32LE(data.length, 20);
    entry.writeUInt32LE(raw.length, 24);
    entry.writeUInt16LE(nameBytes.length, 28);
    entry.writeUInt32LE(offset, 42);
    locals.push(local, nameBytes, data);
    central.push(entry, nameBytes);
    offset += local.length + nameBytes.length + data.length;
  }
  const directory = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(files.size, 8);
  end.writeUInt16LE(files.size, 10);
  end.writeUInt32LE(directory.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, directory, end]);
}

export function unzip(buffer) {
  const bytes = Buffer.from(buffer);
  let end = bytes.length - 22;
  while (end >= 0 && bytes.readUInt32LE(end) !== 0x06054b50) end -= 1;
  assert.ok(end >= 0, 'an end of central directory record');
  const count = bytes.readUInt16LE(end + 10);
  let at = bytes.readUInt32LE(end + 16);
  const files = new Map();
  for (let i = 0; i < count; i += 1) {
    assert.equal(bytes.readUInt32LE(at), 0x02014b50);
    const method = bytes.readUInt16LE(at + 10);
    const compressed = bytes.readUInt32LE(at + 20);
    const nameLength = bytes.readUInt16LE(at + 28);
    const extra = bytes.readUInt16LE(at + 30);
    const comment = bytes.readUInt16LE(at + 32);
    const local = bytes.readUInt32LE(at + 42);
    const name = bytes.subarray(at + 46, at + 46 + nameLength).toString('utf8');
    const start = local + 30 + bytes.readUInt16LE(local + 26) + bytes.readUInt16LE(local + 28);
    const data = bytes.subarray(start, start + compressed);
    files.set(name, method === 8 ? inflateRawSync(data) : Buffer.from(data));
    at += 46 + nameLength + extra + comment;
  }
  return files;
}

/**
 * CF12 review round 1: a request body of `size` bytes (spaces) with no
 * Content-Length (sent chunked, with `duplex: 'half'`). `pulled()` is how
 * many bytes the receiver has asked for so far.
 *
 * CF12b: with `holdAfter`, the body stops once that many bytes are sent
 * (until `release()`). A test of a refusal past a cap sends a little more
 * than the cap and waits: workerd answers early and closes, and a client
 * still writing then could fail with EPIPE instead of reading the answer
 * (seen on /mcp and /internal/bootstrap). The refusal is the same.
 */
export function chunked(size, step = 65_536, holdAfter = Infinity) {
  let sent = 0;
  let release;
  const released = new Promise((done) => {
    release = done;
  });
  const stream = new ReadableStream({
    async pull(controller) {
      if (sent >= holdAfter) {
        await released;
        controller.close();
        return;
      }
      if (sent >= size) {
        controller.close();
        return;
      }
      const n = Math.min(step, size - sent);
      sent += n;
      controller.enqueue(new Uint8Array(n).fill(0x20));
    },
  });
  return { stream, pulled: () => sent, release };
}
