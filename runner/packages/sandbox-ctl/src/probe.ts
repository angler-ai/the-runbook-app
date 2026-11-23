import type { ExecRequest, ExecResponse } from '@runbook/runner-shared/contracts/internal';
import { HttpError } from '@runbook/runner-shared/http';
import { NODE } from './template.ts';

// Exit 20 denotes an open connection. A listener refusal is expected;
// a broken probe or unreachable listener is a separate startup failure.
export const PROBE_ARGV = [NODE, '-e', String.raw`
const net = require('node:net');
const http = require('node:http');
const dns = new (require('node:dns').Resolver)({ timeout: 1000, tries: 1 });
const hosts = ['169.254.169.254', 'metadata.google.internal', '169.254.170.2', '169.254.170.23', 'fd00:ec2::254', '1.1.1.1', ...process.argv.slice(1)];
function direct(host, port) {
  return new Promise((resolve) => {
    const socket = net.connect({host, port});
    const done = (ok) => { socket.destroy(); resolve(ok); };
    socket.once('connect', () => done(true));
    socket.once('error', () => done(false));
    socket.setTimeout(1000, () => done(false));
  });
}
function through(proxy, host, port) {
  return new Promise((resolve) => {
    const authority = (host.includes(':') ? '[' + host + ']' : host) + ':' + port;
    const password = decodeURIComponent(proxy.password);
    const req = http.request({ host: proxy.hostname, port: proxy.port, method: 'CONNECT', path: authority,
      agent: false, headers: { Host: authority,
        'Proxy-Authorization': 'Basic ' + Buffer.from('runbook:' + password).toString('base64') } });
    req.once('connect', (response, socket) => {
      socket.destroy();
      resolve(response.statusCode >= 200 && response.statusCode < 300 ? 20 : response.statusCode === 403 ? 0 : 1);
    });
    req.once('error', () => resolve(1));
    req.setTimeout(2000, () => req.destroy());
    req.end();
  });
}
(async () => {
  const proxy = new URL(process.env.HTTPS_PROXY);
  const fs = require('node:fs');
  if (!fs.statSync(process.env.SSL_CERT_FILE).isFile() ||
      !/^Seccomp:\s+2$/m.test(fs.readFileSync('/proc/self/status', 'utf8'))) throw new Error();
  const tests = hosts.flatMap((host) => [80, 443].map((port) =>
    [direct(host, port).then((open) => open ? 20 : 0), through(proxy, host, port)]));
  const lookup = new Promise((resolve) => dns.resolve4('example.com', (error, records) =>
    resolve(!error && records.length ? 20 : 0)));
  const results = await Promise.all([...tests.flat(), lookup]);
  process.exitCode = results.includes(20) ? 20 : results.includes(1) ? 1 : 0;
})().catch(() => { process.exitCode = 1; });
`];

export async function enforceProbe(exec: (request: ExecRequest) => Promise<ExecResponse>,
  destroy: () => Promise<void>, alert: () => Promise<void>, bridgeGateway?: string): Promise<void> {
  const exit = await exec({ argv: bridgeGateway ? [...PROBE_ARGV, bridgeGateway] : PROBE_ARGV, cwd: '/workspace', timeout_s: 6 })
    .then(({ exit_code }) => exit_code, () => 1);
  if (exit === 0) return;
  if (exit === 20) {
    try { await destroy(); } finally { await alert(); }
    throw new HttpError(503, 'sandbox.probe_succeeded', 'sandbox isolation probe reached a prohibited destination');
  }
  await destroy();
  throw new HttpError(503, 'sandbox.probe_failed', 'sandbox isolation probe did not complete');
}
