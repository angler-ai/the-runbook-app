// Kubernetes remotecommand v4 has no stdin half-close. Read one bounded,
// length-prefixed request and supply finite stdin to the tool ourselves.
import { spawn } from 'node:child_process';
import { writeFileSync } from 'node:fs';

const ca = '/tmp/.runbook-ca.pem';

async function receive() {
  let size;
  let bytes = Buffer.alloc(0);
  for await (const chunk of process.stdin) {
    bytes = Buffer.concat([bytes, chunk]);
    if (size === undefined && bytes.length >= 4) size = bytes.readUInt32BE(0);
    if (size > 1024 * 1024 || bytes.length > 1024 * 1024 + 4) break;
    if (size !== undefined && bytes.length === size + 4) {
      process.stdin.destroy();
      return JSON.parse(bytes.subarray(4).toString('utf8'));
    }
  }
  throw new Error('invalid sandbox request');
}

async function main() {
  const input = await receive();
  if (process.argv[2] === 'init') {
    writeFileSync(ca, input.ca_pem, { flag: 'wx', mode: 0o400 });
    return;
  }
  const proxy = new URL(input.proxy_url);
  proxy.username = 'runbook';
  proxy.password = input.listener_secret;
  const env = {
    ...process.env,
    HTTPS_PROXY: proxy.href, HTTP_PROXY: proxy.href, https_proxy: proxy.href, http_proxy: proxy.href,
    NO_PROXY: '', no_proxy: '', ALL_PROXY: '', all_proxy: '', HOME: '/workspace',
    SSL_CERT_FILE: ca, CURL_CA_BUNDLE: ca, REQUESTS_CA_BUNDLE: ca, NODE_EXTRA_CA_CERTS: ca,
    GIT_SSL_CAINFO: ca, PIP_CERT: ca,
  };
  const child = spawn('/usr/bin/timeout',
    ['--kill-after=1s', '--', `${input.timeout_s}s`, ...input.argv],
    { cwd: input.cwd, env, stdio: ['pipe', 'inherit', 'inherit'] });
  child.stdin.on('error', () => {});
  child.stdin.end(input.stdin);
  child.once('error', () => { process.exitCode = 125; });
  child.once('exit', (code) => { process.exitCode = code ?? 137; });
}

main().catch(() => { process.exitCode = 125; });
