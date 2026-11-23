import { activeContext, clientKind, inSpan, scopedSpan, serverKind, stripTraceHeaders } from '@runbook/runner-shared/telemetry';
import { randomBytes, timingSafeEqual } from 'node:crypto';
import http from 'node:http';
import https from 'node:https';
import { TLSSocket } from 'node:tls';
import type { Socket } from 'node:net';
import type { GrantClaims } from '@runbook/runner-shared/contracts/core';
import type { EgressPolicy } from '@runbook/runner-shared/contracts/internal';
import type { MaterialStore } from '../material.ts';
import { sandboxCA } from './tls.ts';
import { checkAuthority, checkMethod, connectHost, deny, pinnedAddress, policyForHost, type HostRules } from './rules.ts';
import { injectHeader } from './inject.ts';
import { responseGuard } from './guard.ts';


export class ListenerSecret {
  readonly value = randomBytes(32).toString('base64url');

  get header(): string {
    return `Basic ${Buffer.from(`runbook:${this.value}`).toString('base64')}`;
  }

  accepts(header: string | undefined): boolean {
    const expected = Buffer.from(this.header);
    const supplied = Buffer.from(header ?? '');
    return supplied.length === expected.length && timingSafeEqual(supplied, expected);
  }
}


const requestSpanLimit = 100;
const hopHeaders = ['connection', 'keep-alive', 'proxy-authenticate', 'proxy-authorization', 'transfer-encoding',
  'te', 'trailer', 'upgrade', 'content-length', 'expect'];

function endToEnd(raw: http.IncomingHttpHeaders): Headers {
  const headers = new Headers();
  const excluded = new Set([...hopHeaders, ...(raw.connection ?? '').toLowerCase().split(',').map((part) => part.trim())]);
  for (const [name, value] of Object.entries(raw)) {
    if (value !== undefined && !excluded.has(name)) headers.set(name, Array.isArray(value) ? value.join(', ') : value);
  }
  return headers;
}

async function readBounded(stream: http.IncomingMessage, max: number): Promise<Buffer> {
  const chunks: Buffer[] = [];
  let length = 0;
  for await (const part of stream) {
    const chunk = part as Buffer;
    length += chunk.length;
    if (length > max) deny();
    chunks.push(chunk);
  }
  return Buffer.concat(chunks);
}

export async function sandboxListener(options: {
  claims: GrantClaims;
  store: MaterialStore;
  layers: readonly HostRules[];
  policy?: () => Promise<EgressPolicy>;
  listenHost: string;
  publicHost: string;
  port?: number;
  onClose?: () => void;
}) {
  const { store } = options;
  const parent = activeContext();
  let claims = options.claims;
  let expires = claims.exp * 1000;
  const secret = new ListenerSecret();
  const ca = await sandboxCA();
  async function policy(): Promise<EgressPolicy> {
    return options.policy ? options.policy() : { layers: options.layers, destructive: false, credentialLabels: {} };
  }
  async function proxyPolicy(host: string) {
    const current = await policy();
    if (claims.reach === 'read' && (!claims.network?.includes(host) ||
        !current.layers.every((rules) => Object.hasOwn(rules, host) && rules[host] === 'read'))) deny();
    const releases = store.forStep(claims);
    const candidates = releases.filter(({ binding, delivery, credential_id }) => {
      const label = Object.hasOwn(current.credentialLabels, credential_id) && current.credentialLabels[credential_id];
      const allowed = claims.reach === 'read'
        ? label === 'read' && claims.connections?.includes(credential_id)
        : label === 'read' || label === 'write' || (label === 'destructive' && current.destructive === true);
      return delivery === 'proxy' && allowed &&
        Array.isArray(binding.hosts) && binding.hosts.includes(host);
    });
    if (candidates.length !== 1) deny();
    const release = candidates[0]!;
    const access = policyForHost(host, current.layers, release.binding.hosts as string[]);
    return { releases, release, access };
  }
  const tunnels = new WeakMap<Socket, { host: string; address: Awaited<ReturnType<typeof pinnedAddress>> }>();
  const sockets = new Set<Socket>();
  async function proxyRequest(req: http.IncomingMessage, res: http.ServerResponse, traced: boolean) {
    try {
      const socket = req.socket as TLSSocket & { servername?: string };
      const { host, address } = tunnels.get(socket) ?? deny();
      checkAuthority(host, socket.servername, req.headers.host);
      const path = req.url ?? '';
      if (!/^\/(?![/\\])/.test(path) || /[\\#]/.test(path)) deny();
      const method = req.method ?? deny();
      const body = await readBounded(req, 2 * 1024 * 1024);
      const { releases, release, access } = await proxyPolicy(host);
      checkMethod(access, method);
      const key = release.material.fields.key ?? release.material.fields.access_token;
      if (!key) deny();
      const headers = endToEnd(req.headers);
      headers.set('host', host);
      headers.set('accept-encoding', 'identity');
      if (body.length) headers.set('content-length', String(body.length));
      injectHeader(headers, release.binding, key);

      stripTraceHeaders(headers);
      const send = () => new Promise<http.IncomingMessage>((resolve, reject) => {
        const outbound = https.request({
          hostname: address.address, family: address.family, servername: host, method, path,
          headers: Object.fromEntries(headers), agent: false, signal: AbortSignal.timeout(30_000),
        }, resolve);
        outbound.once('error', reject);
        outbound.end(body);
      });
      const response = await (traced ? inSpan('egress.http.client', send, clientKind) : send());
      if (response.headers['content-encoding'] && response.headers['content-encoding'] !== 'identity') {
        response.destroy();
        deny();
      }
      const bytes = await readBounded(response, 8 * 1024 * 1024);
      const values = [...releases, ...store.forStep(claims)].flatMap(({ material }) => Object.values(material.fields));
      const guard = responseGuard(values);
      const safe = guard(bytes);
      const outboundHeaders = endToEnd(response.headers);
      for (const [name, value] of outboundHeaders) {
        const header = Buffer.from(name);
        if (guard(header) !== header) outboundHeaders.delete(name);
        else outboundHeaders.set(name, guard(Buffer.from(value)).toString());
      }
      outboundHeaders.set('content-length', String(safe.length));
      res.writeHead(response.statusCode ?? 502, Object.fromEntries(outboundHeaders));
      res.end(safe);
    } catch {
      res.writeHead(403, { connection: 'close' }).end('egress denied');
    }
  }
  let stepSpan: ReturnType<typeof scopedSpan> | undefined;
  let requests = 0;
  const tlsHttp = http.createServer((req, res) => {
    stepSpan ??= scopedSpan('egress.step', parent);
    const traced = ++requests <= requestSpanLimit && stepSpan.span.isRecording();
    const response = traced
      ? inSpan('egress.request', () => proxyRequest(req, res, true), serverKind, stepSpan.context)
      : proxyRequest(req, res, false);
    void response.catch(() => res.destroy());
  });
  tlsHttp.on('upgrade', (_req, socket) => socket.destroy());
  tlsHttp.on('connect', (_req, socket) => socket.destroy());
  const listener = http.createServer((_req, res) => res.writeHead(405, { connection: 'close' }).end());
  listener.on('connect', (req, socket, head) => {
    void (async () => {
      try {
        if (Date.now() >= expires || !secret.accepts(req.headers['proxy-authorization'])) {
          socket.end('HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm="Runbook"\r\nConnection: close\r\n\r\n');
          return;
        }
        const host = connectHost(req.url ?? '');
        if (connectHost(req.headers.host ?? '') !== host) deny();
        await proxyPolicy(host);
        const address = await pinnedAddress(host);
        if (socket.destroyed || Date.now() >= expires) return socket.destroy();
        const secureContext = ca.context(host, expires);
        socket.write('HTTP/1.1 200 Connection Established\r\n\r\n');
        if (head.length) socket.unshift(head);
        const secured = new TLSSocket(socket, {
          isServer: true, secureContext,
          SNICallback(servername, callback) {
            if (servername.toLowerCase() === host) callback(null, secureContext);
            else callback(new Error('egress denied'));
          },
        });
        tunnels.set(secured, { host, address });
        secured.on('error', () => secured.destroy());
        secured.setTimeout(30_000, () => secured.destroy());
        tlsHttp.emit('connection', secured);
      } catch {
        socket.end('HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n');
      }
    })();
  });
  listener.on('connection', (socket) => {
    sockets.add(socket);
    socket.once('close', () => sockets.delete(socket));
  });

  let timer: NodeJS.Timeout | undefined;
  let closed = false;
  const close = () => {
    if (closed) return;
    closed = true;
    clearTimeout(timer);
    if (stepSpan) {
      stepSpan.span.setAttribute('egress.requests_suppressed', Math.max(0, requests - requestSpanLimit));
      stepSpan.span.end();
    }
    options.onClose?.();
    listener.close();
    for (const socket of sockets) socket.destroy();
  };
  await new Promise<void>((resolve, reject) => {
    listener.once('error', reject);
    listener.listen(options.port ?? 0, options.listenHost, () => {
      listener.removeListener('error', reject);
      resolve();
    });
  });
  const scheduleExpiry = () => {
    clearTimeout(timer);
    timer = setTimeout(close, Math.max(0, expires - Date.now())).unref();
  };
  scheduleExpiry();
  const address = listener.address();
  if (!address || typeof address === 'string') {
    close();
    deny();
  }
  return {
    close,
    renew(next: GrantClaims) {
      if (closed || expires <= Date.now() || next.exp < claims.exp) return false;
      claims = next;
      expires = next.exp * 1000;
      scheduleExpiry();
      return true;
    },
    proxy_url: `http://${options.publicHost}:${address.port}`,
    listener_secret: secret.value,
    ca_pem: ca.pem,
  };
}
