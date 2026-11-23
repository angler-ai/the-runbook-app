import type { ExecRequest, ExecResponse } from '@runbook/runner-shared/contracts/internal';
import { HttpError } from '@runbook/runner-shared/http';
import { NODE } from './template.ts';

export const KUBERNETES_RUNTIME_PROBE = [NODE, '-e', String.raw`
const { execFileSync } = require('node:child_process');
const log = execFileSync('/usr/bin/dmesg', { encoding: 'utf8', maxBuffer: 131072 });
if (!log.includes('Starting gVisor')) process.exit(1);
`];
const CONNECTIVITY = [NODE, '-e', String.raw`
const net = require('node:net');
const http = require('node:http');
const { Resolver } = require('node:dns').promises;
const proxy = new URL(process.env.HTTPS_PROXY);
const authorization = 'Basic ' + Buffer.from('runbook:' + decodeURIComponent(proxy.password)).toString('base64');
const blocked = [
  ['169.254.169.254', 80, 443], ['169.254.169.252', 987, 988],
  ['metadata.google.internal', 80, 443], ['169.254.170.2', 80, 443],
  ['169.254.170.23', 80, 443], ['fd00:ec2::254', 80, 443],
  ['1.1.1.1', 443],
];
function direct(host, port) {
  return new Promise((resolve) => {
    const socket = net.connect({ host, port });
    const done = (ok) => { socket.destroy(); resolve(ok); };
    socket.once('connect', () => done(true));
    socket.once('error', () => done(false));
    socket.setTimeout(1000, () => done(false));
  });
}
function through(host, port, auth) {
  return new Promise((resolve) => {
    const authority = (host.includes(':') ? '[' + host + ']' : host) + ':' + port;
    const req = http.request({ host: proxy.hostname, port: proxy.port, method: 'CONNECT', path: authority,
      agent: false, headers: { Host: authority, ...(auth && { 'Proxy-Authorization': authorization }) } });
    req.once('connect', (response, socket) => { socket.destroy(); resolve(response.statusCode); });
    req.once('error', () => resolve(0));
    req.setTimeout(2000, () => req.destroy());
    req.end();
  });
}
(async () => {
  if (!require('node:fs').statSync(process.env.SSL_CERT_FILE).isFile()) throw new Error();
  const targets = blocked.flatMap(([host, ...ports]) => ports.map((port) => [host, port]));
  const resolver = new Resolver({ timeout: 1000, tries: 1 });
  const lookup = Promise.all([
    resolver.resolve4('example.com'), resolver.resolve4('kubernetes.default.svc.cluster.local'),
  ].map((request) => request.then((records) => records.length > 0, () => false)));
  const [unauthenticated, directResults, proxyResults, dnsReached] = await Promise.all([
    through('169.254.169.254', 80, false),
    Promise.all(targets.filter(([host]) => net.isIP(host)).map(([host, port]) => direct(host, port))),
    Promise.all(targets.map(([host, port]) => through(host, port, true))),
    lookup,
  ]);
  if (dnsReached.some(Boolean) || directResults.some(Boolean) || proxyResults.some((code) => code >= 200 && code < 300)) process.exitCode = 20;
  else if (unauthenticated !== 407 || proxyResults.some((code) => code !== 403)) process.exitCode = 1;
})().catch(() => { process.exitCode = 1; });
`];

export async function enforceKubernetesProbe(exec: (request: ExecRequest) => Promise<ExecResponse>,
  alert: () => Promise<void>) {
  const run = (argv: string[]) => exec({ argv, cwd: '/workspace', timeout_s: 6 })
    .then(({ exit_code }) => exit_code, () => 1);
  const connectivity = await run(CONNECTIVITY);
  if (connectivity === 20) {
    await alert().catch(() => undefined);
    throw new HttpError(503, 'sandbox.probe_succeeded', 'sandbox isolation probe reached a prohibited destination');
  }
  if (connectivity !== 0) throw new HttpError(503, 'sandbox.probe_failed', 'sandbox isolation probe did not complete');
}
