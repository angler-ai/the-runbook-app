import type Docker from 'dockerode';
import type { SandboxListenerResponse, SandboxSize } from '@runbook/runner-shared/contracts/internal';

export const SANDBOX_LABEL = 'runbook.sandbox';
export const EXIT_MARGIN_MS = 3000;
export const TRUSTED_HELPER = '/runbook-trusted/helper';
export const NODE = '/usr/local/bin/node';
export const SIZES = {
  small: { vcpu: 0.5, gib: 1 },
  medium: { vcpu: 1, gib: 2 },
  large: { vcpu: 2, gib: 4 },
} as const;

export function template(id: string, network: string, image: string, size: SandboxSize,
  listener: SandboxListenerResponse, timeoutMs: number,
  sandboxLabel = SANDBOX_LABEL): Docker.ContainerCreateOptions {
  const profile = SIZES[size];
  const proxy = new URL(listener.proxy_url);
  proxy.username = 'runbook';
  proxy.password = listener.listener_secret;
  const ca = '/workspace/.runbook-ca.pem';
  return {
    name: id, Image: image, User: '10001:10001', WorkingDir: '/workspace',
    Entrypoint: [NODE, '-e'], Healthcheck: { Test: ['NONE'] },
    Cmd: ['require("node:fs").writeFileSync(process.env.SSL_CERT_FILE, process.env.RUNBOOK_SANDBOX_CA, { mode: 0o400 }); setTimeout(() => {}, Number(process.argv[1]));', String(timeoutMs + EXIT_MARGIN_MS)],
    Env: [
      `HTTPS_PROXY=${proxy.href}`, `HTTP_PROXY=${proxy.href}`, `https_proxy=${proxy.href}`, `http_proxy=${proxy.href}`,
      'NO_PROXY=', 'no_proxy=', 'ALL_PROXY=', 'all_proxy=', 'HOME=/workspace',
      `SSL_CERT_FILE=${ca}`, `CURL_CA_BUNDLE=${ca}`, `REQUESTS_CA_BUNDLE=${ca}`, `NODE_EXTRA_CA_CERTS=${ca}`,
      `GIT_SSL_CAINFO=${ca}`, `PIP_CERT=${ca}`,
      `RUNBOOK_SANDBOX_CA=${listener.ca_pem}`,
    ],
    Labels: { [sandboxLabel]: '1' },
    HostConfig: {
      AutoRemove: false, Init: true, ReadonlyRootfs: true, NetworkMode: network,
      CapDrop: ['ALL'], SecurityOpt: ['no-new-privileges', 'seccomp=builtin'], PidsLimit: 128,
      NanoCpus: profile.vcpu * 1e9, Memory: profile.gib * 2 ** 30, MemorySwap: profile.gib * 2 ** 30,
      Tmpfs: {
        '/workspace': 'rw,nosuid,nodev,exec,size=256m,uid=10001,gid=10001,mode=0700',
        '/tmp': 'rw,nosuid,nodev,noexec,size=64m,mode=1777',
        '/runbook-results': 'rw,nosuid,nodev,noexec,size=64m,uid=10001,gid=10001,mode=0700',
      },
      LogConfig: { Type: 'none', Config: {} },
    },
  };
}

// Keep directory descriptors open throughout resolution so replacing an
// intermediate path with a symlink cannot redirect a read or write.
export const FILE_HELPER = String.raw`
const fs = require('node:fs');
const { O_RDONLY, O_DIRECTORY, O_NOFOLLOW, O_NONBLOCK, O_WRONLY, O_CREAT, O_TRUNC } = fs.constants;
try {
  const [operation, filename] = process.argv.slice(1);
  let parent = fs.openSync('/workspace', O_RDONLY | O_DIRECTORY);
  const parts = filename.split('/');
  const leaf = parts.pop();
  for (const part of parts) {
    const next = fs.openSync('/proc/self/fd/' + parent + '/' + part, O_RDONLY | O_DIRECTORY | O_NOFOLLOW);
    fs.closeSync(parent);
    parent = next;
  }
  const dir = '/proc/self/fd/' + parent + '/';
  if (operation === 'read') {
    const file = fs.openSync(dir + leaf, O_RDONLY | O_NOFOLLOW | O_NONBLOCK);
    const info = fs.fstatSync(file);
    if (!info.isFile() || info.size > 131072) throw new Error();
    const bytes = Buffer.alloc(131073);
    const length = fs.readSync(file, bytes, 0, bytes.length, 0);
    if (length > 131072) throw new Error();
    process.stdout.write(bytes.subarray(0, length));
  } else {
    const tmp = '.runbook-write-' + require('node:crypto').randomUUID();
    const file = fs.openSync(dir + tmp, O_WRONLY | O_CREAT | O_TRUNC | fs.constants.O_EXCL | O_NOFOLLOW, 0o600);
    try {
      fs.writeFileSync(file, fs.readFileSync(0));
      fs.renameSync(dir + tmp, dir + leaf);
    } finally {
      fs.closeSync(file);
      try { fs.unlinkSync(dir + tmp); } catch {}
    }
  }
} catch { process.exitCode = 1; }
`;
