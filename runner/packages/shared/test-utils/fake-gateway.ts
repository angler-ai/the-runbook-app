// A fake gateway for vault tests (the real one is chunk C): it takes
// material pushes and deletes from the vault, and runs connection tests by
// sending the request to a local upstream with the credential's declared
// header set. It is deliberately leaky: its test answers include the
// upstream's body, so tests can prove the vault never passes it on.
import http from 'node:http';
import type { AddressInfo } from 'node:net';
import type { ClaimFence, GatewayTestRequest, MaterialPush } from '../src/contracts/internal.ts';

export class FakeGateway {
  url = '';
  readonly token: string;
  readonly material = new Map<string, MaterialPush>();
  readonly pushes: MaterialPush[] = [];
  readonly deletes: string[] = [];
  /** Where test requests really go (the URL's origin is replaced). */
  upstream: string | undefined;
  /** Simulate a network failure class for tests. */
  testOutcome: 'unreachable' | 'tls' | undefined;
  failPushes = false;
  private readonly server: http.Server;

  constructor(token = 'fake-token-vault-to-gateway-000000000000') {
    this.token = token;
    this.server = http.createServer((req, res) => void this.handle(req, res));
  }

  async start(): Promise<this> {
    await new Promise<void>((resolve) => this.server.listen(0, '127.0.0.1', resolve));
    this.url = `http://127.0.0.1:${(this.server.address() as AddressInfo).port}`;
    return this;
  }

  async close(): Promise<void> {
    this.server.closeAllConnections();
    await new Promise<void>((resolve) => this.server.close(() => resolve()));
  }

  private async handle(req: http.IncomingMessage, res: http.ServerResponse): Promise<void> {
    const chunks: Buffer[] = [];
    for await (const c of req) chunks.push(c as Buffer);
    const text = Buffer.concat(chunks).toString('utf8');
    const path = new URL(req.url ?? '/', 'http://x').pathname;
    const send = (status: number, payload?: unknown) => {
      res.writeHead(status, { 'content-type': 'application/json' });
      res.end(payload === undefined ? '' : JSON.stringify(payload));
    };
    if (req.headers.authorization !== `Bearer ${this.token}`) return send(401, { error: { code: 'auth' } });
    if (req.method === 'POST' && path === '/internal/material') {
      if (this.failPushes) return send(503, { error: { code: 'unavailable' } });
      const push = JSON.parse(text) as MaterialPush;
      this.pushes.push(push);
      this.material.set(push.release_id, push);
      return send(204);
    }
    if (req.method === 'POST' && path === '/internal/material/end-claim') {
      const claim = JSON.parse(text) as ClaimFence;
      for (const [id, release] of this.material) {
        if (release.step_run_id === claim.sub && release.claim_id === claim.claim_id &&
            release.generation === claim.generation && release.runner_id === claim.runner_id) this.material.delete(id);
      }
      return send(204);
    }
    const credential = /^\/internal\/material\/credentials\/([^/]+)$/.exec(path)?.[1];
    if (req.method === 'DELETE' && credential) {
      for (const [id, release] of this.material) {
        if (release.credential_id === decodeURIComponent(credential)) this.material.delete(id);
      }
      return send(204);
    }
    const del = /^\/internal\/material\/([^/]+)$/.exec(path);
    if (req.method === 'DELETE' && del) {
      const id = decodeURIComponent(del[1] ?? '');
      this.deletes.push(id);
      this.material.delete(id);
      return send(204);
    }
    if (req.method === 'POST' && path === '/internal/test') {
      if (this.testOutcome) return send(200, { outcome: this.testOutcome });
      const t = JSON.parse(text) as GatewayTestRequest;
      const m = this.material.get(t.release_id);
      if (!m) return send(404, { error: { code: 'material.unknown' } });
      const binding = m.binding as { header_name?: string; header_format?: string };
      const value = Object.values(m.material.fields)[0] ?? '';
      const target = new URL(t.request.url);
      const upstream = new URL(target.pathname, this.upstream ?? target.origin);
      const headers: Record<string, string> = {};
      if (binding.header_name) headers[binding.header_name] = (binding.header_format ?? '{}').replace('{}', value);
      const r = await fetch(upstream, { method: t.request.method, headers });
      // Leaky on purpose: the upstream body (which may echo the key) is returned.
      return send(200, { outcome: 'response', status: r.status, body: await r.text() });
    }
    return send(404, { error: { code: 'route.not_found' } });
  }
}
