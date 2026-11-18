import { readFileSync as readRecorded } from 'node:fs';
const RECORDED_ANSWER = JSON.parse(readRecorded(new URL('../../../api/internal/can-register.v1.response.json', import.meta.url), 'utf8'));
// Test doubles shared by the D1 and Postgres suites. Nothing here talks to the
// network: Google's token and JWKS endpoints are answered from a local RSA key,
// can-register is a loopback HTTP server, and SMTP is a loopback fake.
import { createServer } from 'node:http';
import { createServer as createTcpServer } from 'node:net';
import { generateKeyPairSync, randomUUID, sign } from 'node:crypto';

export const INTERNAL_SECRET = 'test-internal-secret-0123456789abcdef0123456789';
export const AUTH_SECRET = 'test-auth-secret-which-is-at-least-32-bytes-long';
export const GOOGLE_CLIENT_ID = 'test-client.apps.example.test';
export const GOOGLE_CLIENT_SECRET = 'test-google-client-secret';
export const ORIGIN = 'https://runbook.example.test';
/** Allowed domain list for the suites; `blocked.example.test` is outside it. */
export const ALLOWED_DOMAINS = 'example.test';

const listen = (server) =>
  new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server.address().port)));

/** Rust's internal `can-register`, stubbed: allows the addresses in `allowed`. */
export async function startCanRegister() {
  const state = { allowed: new Set(), calls: [], status: 200 };
  const server = createServer((req, res) => {
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => {
      let email = null;
      try {
        email = JSON.parse(body).email;
      } catch {
        /* recorded as null */
      }
      state.calls.push({ method: req.method, path: req.url, secret: req.headers['x-runbook-internal-secret'] ?? null, email });
      if (state.status !== 200) {
        res.writeHead(state.status, { 'content-type': 'application/json' });
        return res.end('{}');
      }
      const ok = req.headers['x-runbook-internal-secret'] === INTERNAL_SECRET && state.allowed.has(email);
      res.writeHead(200, { 'content-type': 'application/json' });
      // UPD1 review #8: the recorded answer shape (api/internal/), plus a
      // field a newer api release might add, which the service must ignore.
      res.end(JSON.stringify({ ...RECORDED_ANSWER, allowed: ok, a_field_from_a_newer_release: 1 }));
    });
  });
  const port = await listen(server);
  return { state, url: `http://127.0.0.1:${port}/internal/auth/can-register`, origin: `http://127.0.0.1:${port}`, close: () => new Promise((r) => server.close(r)) };
}

/** Google's OAuth token endpoint and JWKS, answered with a local test key. */
export function googleMock() {
  const { privateKey, publicKey } = generateKeyPairSync('rsa', { modulusLength: 2048 });
  const rogue = generateKeyPairSync('rsa', { modulusLength: 2048 }).privateKey;
  const kid = `test-${randomUUID()}`;
  const jwk = { ...publicKey.export({ format: 'jwk' }), kid, alg: 'RS256', use: 'sig' };
  const b64 = (v) => Buffer.from(typeof v === 'string' ? v : JSON.stringify(v)).toString('base64url');
  const state = { next: null, calls: [] };
  const signJwt = (claims, key = privateKey) => {
    const input = `${b64({ alg: 'RS256', kid, typ: 'JWT' })}.${b64(claims)}`;
    return `${input}.${sign('sha256', Buffer.from(input), key).toString('base64url')}`;
  };
  return {
    state,
    /** Returns {status, headers, body} for Google URLs, or null for anything else. */
    async handle(url, method, bodyText) {
      if (url.origin === 'https://oauth2.googleapis.com' && url.pathname === '/token' && method === 'POST') {
        state.calls.push('token');
        const form = new URLSearchParams(bodyText);
        if (!state.next || form.get('client_id') !== GOOGLE_CLIENT_ID || form.get('grant_type') !== 'authorization_code') {
          return { status: 400, headers: { 'content-type': 'application/json' }, body: JSON.stringify({ error: 'invalid_grant' }) };
        }
        const now = Math.floor(Date.now() / 1000);
        const p = state.next;
        const idToken = signJwt({ iss: 'https://accounts.google.com', aud: GOOGLE_CLIENT_ID, sub: p.sub, email: p.email, email_verified: p.email_verified, name: p.name ?? 'Test Person', iat: now, exp: now + 3600, ...(p.nonce ? { nonce: p.nonce } : {}) }, p.forge ? rogue : privateKey);
        return { status: 200, headers: { 'content-type': 'application/json' }, body: JSON.stringify({ access_token: 'test-access-token', id_token: idToken, token_type: 'Bearer', expires_in: 3600, scope: 'openid email profile' }) };
      }
      if (url.href === 'https://www.googleapis.com/oauth2/v3/certs') {
        state.calls.push('jwks');
        return { status: 200, headers: { 'content-type': 'application/json' }, body: JSON.stringify({ keys: [jwk] }) };
      }
      return null;
    },
  };
}

/**
 * The single outbound policy for both runtimes: Google URLs go to the mock,
 * can-register to its loopback stub, and everything else is refused and recorded.
 */
export function outboundPolicy({ google, canRegister, realFetch }) {
  const unexpected = [];
  return {
    unexpected,
    async route(url, method, headers, bodyText) {
      const mocked = await google.handle(url, method, bodyText);
      if (mocked) return mocked;
      if (url.origin === canRegister.origin) {
        const r = await realFetch(url, { method, headers, body: bodyText || undefined });
        return { status: r.status, headers: { 'content-type': r.headers.get('content-type') ?? 'application/json' }, body: await r.text() };
      }
      unexpected.push(`${method} ${url.href}`);
      return { status: 502, headers: { 'content-type': 'text/plain' }, body: 'outbound request refused by the test harness' };
    },
  };
}

/**
 * A minimal SMTP server for nodemailer: no TLS, no auth. Records each message's
 * recipient and subject (which carries the code). RCPT to `fail@...` is refused.
 */
export async function startSmtp() {
  const messages = [];
  const server = createTcpServer((socket) => {
    let buffer = '';
    let inData = false;
    let rcpt = null;
    const reply = (line) => socket.write(`${line}\r\n`);
    reply('220 smtp.example.test ESMTP test');
    socket.on('data', (chunk) => {
      buffer += chunk.toString('utf8');
      for (;;) {
        if (inData) {
          const end = buffer.indexOf('\r\n.\r\n');
          if (end === -1) return;
          const data = buffer.slice(0, end);
          buffer = buffer.slice(end + 5);
          inData = false;
          const subject = /^Subject: (.*)$/im.exec(data)?.[1]?.trim() ?? '';
          messages.push({ to: rcpt, subject, code: /^(\d{6}) /.exec(subject)?.[1] ?? null });
          reply('250 2.0.0 queued');
          continue;
        }
        const nl = buffer.indexOf('\r\n');
        if (nl === -1) return;
        const line = buffer.slice(0, nl);
        buffer = buffer.slice(nl + 2);
        const verb = line.slice(0, 4).toUpperCase();
        if (verb === 'EHLO' || verb === 'HELO') socket.write('250-smtp.example.test\r\n250 8BITMIME\r\n');
        else if (verb === 'MAIL') reply('250 2.1.0 ok');
        else if (verb === 'RCPT') {
          rcpt = /<([^>]*)>/.exec(line)?.[1]?.toLowerCase() ?? null;
          reply(rcpt?.startsWith('fail@') ? '550 5.1.1 mailbox unavailable' : '250 2.1.5 ok');
        } else if (verb === 'DATA') {
          inData = true;
          reply('354 end with <CRLF>.<CRLF>');
        } else if (verb === 'QUIT') {
          reply('221 bye');
          socket.end();
        } else if (verb === 'RSET' || verb === 'NOOP') reply('250 ok');
        else reply('502 5.5.2 not implemented');
      }
    });
    socket.on('error', () => {});
  });
  const port = await listen(server);
  return { messages, url: `smtp://127.0.0.1:${port}`, close: () => new Promise((r) => server.close(r)) };
}
