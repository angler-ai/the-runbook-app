// HOST1: DATABASE_URL's TLS and socket keys mean what they mean to
// runbook-server (libpq's meaning), and never reach Postgres as startup
// parameters. No database needed.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { databaseSettings } from '../dist/server.mjs';

const PEM = '-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n';
function settings(url, env = {}, files = {}) {
  const problems = [];
  const read = (path) => {
    if (!(path in files)) throw new Error('ENOENT');
    return files[path];
  };
  return { result: databaseSettings(url, env, read, problems), problems };
}

test('the default is prefer: TLS when offered, like libpq and runbook-server', () => {
  const { result, problems } = settings('postgres://u:p@db.example:5432/rb');
  assert.deepEqual(problems, []);
  assert.equal(result.url, 'postgres://u:p@db.example:5432/rb');
  assert.deepEqual(result.options, { max: 5, ssl: 'prefer' });
  assert.equal(result.mode, 'prefer');
});

test('each sslmode maps to postgres.js TLS options, and the keys leave the URL', () => {
  const cases = [
    ['disable', false],
    ['allow', 'prefer'],
    ['prefer', 'prefer'],
    ['require', 'require'],
  ];
  for (const [mode, ssl] of cases) {
    const { result, problems } = settings(`postgres://u@db.example/rb?sslmode=${mode}&application_name=x`);
    assert.deepEqual(problems, [], mode);
    assert.equal(result.options.ssl, ssl, mode);
    assert.equal(result.url, 'postgres://u@db.example/rb?application_name=x', mode);
  }
  const full = settings('postgres://u@db.example/rb?sslmode=verify-full').result;
  assert.deepEqual(full.options.ssl, { rejectUnauthorized: true, host: 'db.example' });
  const ca = settings('postgres://u@db.example/rb?sslmode=verify-ca').result.options.ssl;
  assert.equal(ca.rejectUnauthorized, true);
  assert.equal(ca.checkServerIdentity('db.example', {}), undefined);
  assert.equal(ca.ca, undefined);
});

test('sslrootcert replaces the roots; require with a root checks the chain', () => {
  const files = { '/run/secrets/ca.pem': PEM };
  const full = settings('postgres://u@db.example/rb?sslmode=verify-full&sslrootcert=%2Frun%2Fsecrets%2Fca.pem', {}, files).result;
  assert.deepEqual(full.options.ssl, { ca: PEM, rejectUnauthorized: true, host: 'db.example' });
  assert.equal(full.url, 'postgres://u@db.example/rb');
  const req = settings('postgres://u@db.example/rb?sslmode=require&sslrootcert=/run/secrets/ca.pem', {}, files).result;
  assert.equal(req.options.ssl.ca, PEM);
  assert.equal(req.options.ssl.rejectUnauthorized, true);
  assert.equal(typeof req.options.ssl.checkServerIdentity, 'function');
  const system = settings('postgres://u@db.example/rb?sslmode=verify-full&sslrootcert=system').result;
  assert.deepEqual(system.options.ssl, { rejectUnauthorized: true, host: 'db.example' });
});

test('a Cloud SQL socket needs no host in the URL and never uses TLS', () => {
  const { result, problems } = settings('postgres://u:p@/rb?host=/cloudsql/proj:region:inst&sslmode=require');
  assert.deepEqual(problems, []);
  assert.equal(result.socket, true);
  assert.equal(result.options.host, '/cloudsql/proj:region:inst');
  assert.equal(result.options.ssl, false);
  assert.equal(result.url, 'postgres://u:p@localhost/rb');
});

test('the pool size is configurable for small managed tiers', () => {
  assert.equal(settings('postgres://u@h/rb', { RUNBOOK_AUTH_DATABASE_POOL_SIZE: '2' }).result.options.max, 2);
  for (const bad of ['0', '101', 'two', '1.5']) {
    const { result, problems } = settings('postgres://u@h/rb', { RUNBOOK_AUTH_DATABASE_POOL_SIZE: bad });
    assert.equal(result, null, bad);
    assert.equal(problems.length, 1, bad);
  }
});

test('bad values are refused without quoting the URL', () => {
  for (const url of [
    'postgres://u:secret@h/rb?sslmode=verify-everything',
    'postgres://u:secret@h/rb?sslrootcert=',
    'postgres://u:secret@h/rb?sslmode=verify-full&sslrootcert=/missing.pem',
    'postgres://u:secret@h/rb?host=relative',
    'postgres://u:secret@/rb',
    'mysql://u:secret@h/rb',
  ]) {
    const { result, problems } = settings(url, {}, { '/junk.pem': 'junk' });
    assert.equal(result, null, url);
    assert.ok(problems.length > 0, url);
    assert.ok(problems.every((p) => !p.includes('secret')), url);
  }
  const junk = settings('postgres://u@h/rb?sslrootcert=/junk.pem', {}, { '/junk.pem': 'junk' });
  assert.equal(junk.result, null);
});

// HOST1 review: postgres.js leaves `servername` unset for an IP host, and
// Node then checks the certificate against "localhost". verify-full must
// check the URL's host itself, so a real handshake is made here the way
// postgres.js makes it (an existing socket, no servername).
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createConnection } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import tls from 'node:tls';

function selfSigned(dir, san) {
  const config = join(dir, `${san.replace(/\W/g, '_')}.cnf`);
  writeFileSync(config, `[req]\ndistinguished_name=dn\n[dn]\n[san]\nsubjectAltName=${san}\n`);
  const key = join(dir, `${san.replace(/\W/g, '_')}.key`);
  const cert = join(dir, `${san.replace(/\W/g, '_')}.pem`);
  execFileSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', key, '-out', cert, '-days', '1', '-subj', '/CN=db', '-config', config, '-extensions', 'san'], { stdio: 'ignore' });
  return { key: readFileSync(key, 'utf8'), cert: readFileSync(cert, 'utf8'), certPath: cert };
}

async function handshake(server, url) {
  const port = server.address().port;
  const { result, problems } = settings(url.replace('PORT', String(port)), {}, new Proxy({}, { has: () => true, get: (_, path) => readFileSync(path, 'utf8') }));
  assert.deepEqual(problems, []);
  return new Promise((resolve) => {
    const socket = createConnection({ host: '127.0.0.1', port }, () => {
      const secure = tls.connect({ socket, servername: undefined, ...result.options.ssl });
      secure.once('secureConnect', () => { secure.end(); resolve('ok'); });
      secure.once('error', (error) => { socket.destroy(); resolve(error.code || error.message); });
    });
  });
}

test('verify-full checks an IP-literal host against the certificate', async (t) => {
  try {
    execFileSync('openssl', ['version'], { stdio: 'ignore' });
  } catch {
    t.skip('openssl is not installed');
    return;
  }
  const dir = mkdtempSync(join(tmpdir(), 'rb-auth-tls-'));
  try {
    const right = selfSigned(dir, 'IP:127.0.0.1');
    const wrong = selfSigned(dir, 'IP:127.0.0.2');
    for (const [pair, expected] of [[right, 'ok'], [wrong, 'ERR_TLS_CERT_ALTNAME_INVALID']]) {
      const server = tls.createServer({ key: pair.key, cert: pair.cert }, (s) => s.end());
      await new Promise((r) => server.listen(0, '127.0.0.1', r));
      try {
        const outcome = await handshake(server, `postgres://u@127.0.0.1:PORT/rb?sslmode=verify-full&sslrootcert=${encodeURIComponent(pair.certPath)}`);
        assert.equal(outcome, expected);
      } finally {
        await new Promise((r) => server.close(r));
      }
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('verify-full names the URL host for the identity check', () => {
  assert.equal(settings('postgres://u@10.1.2.3/rb?sslmode=verify-full').result.options.ssl.host, '10.1.2.3');
  assert.equal(settings('postgres://u@[2001:db8::1]/rb?sslmode=verify-full').result.options.ssl.host, '2001:db8::1');
  assert.equal(settings('postgres://u@db.example/rb?sslmode=verify-full').result.options.ssl.host, 'db.example');
});
