import { tracedServer, setLabels } from '@runbook/runner-shared/telemetry';
import http from 'node:http';
import type { UserAssertionClaims } from '@runbook/runner-shared/contracts/core';
import { HttpError, sameToken } from '@runbook/runner-shared/http';
import type { AssertionVerifier } from './auth.ts';

export interface PublicRequest {
  method: string;
  target: string;
  claims: UserAssertionClaims;
  body: unknown;
  assertion?: { token: string; rawBody: Buffer };
}

export function createApiServer(
  verifier: Pick<AssertionVerifier, 'verifyAssertion'>,
  handle: (request: PublicRequest) => Promise<{ status?: number; body?: unknown }>,
  webhook?: http.RequestListener,
  metrics?: () => Promise<string>,
  metricsToken?: string,
): http.Server {
  return http.createServer(tracedServer(async (req, res) => {
    const target = req.url ?? '';
    if (metrics && req.method === 'GET' && target === '/metrics') {
      if (!metricsToken || !req.headers.authorization || !sameToken(req.headers.authorization, `Bearer ${metricsToken}`)) {
        res.writeHead(401).end();
        return;
      }
      try {
        const body = await metrics();
        res.writeHead(200, { 'content-type': 'text/plain; version=0.0.4' }).end(body);
      } catch {
        res.writeHead(503).end();
      }
      return;
    }
    if (webhook && target === '/runner/v1/webhooks/core') {
      webhook(req, res);
      return;
    }
    const send = (status: number, body?: unknown) => {
      res.writeHead(status, { 'content-type': 'application/json', 'cache-control': 'no-store' });
      res.end(body === undefined ? undefined : JSON.stringify(body));
    };
    try {
      if (!target.startsWith('/runner/v1/') || !req.method) throw new HttpError(404, 'route.not_found', 'no such route');
      const token = req.headers['runbook-assertion'];
      if (typeof token !== 'string') throw new HttpError(401, 'assertion.invalid', 'invalid runner assertion');
      const chunks: Buffer[] = [];
      let length = 0;
      for await (const part of req) {
        const chunk = part as Buffer;
        length += chunk.length;
        if (length > 256 * 1024) throw new HttpError(413, 'request.too_large', 'the request body is too large');
        chunks.push(chunk);
      }
      const bytes = Buffer.concat(chunks);
      const claims = await verifier.verifyAssertion({ token, method: req.method, target, body: bytes });
      setLabels({ org: claims.org_id });
      let body: unknown;
      if (bytes.length) {
        try {
          body = JSON.parse(bytes.toString('utf8')) as unknown;
        } catch {
          throw new HttpError(400, 'request.invalid_json', 'the request body is not valid JSON');
        }
      }
      const result = await handle({ method: req.method, target, claims, body, assertion: { token, rawBody: bytes } });
      send(result.status ?? 200, result.body);
    } catch (error) {
      if (error instanceof HttpError) send(error.status, { error: { code: error.code, message: error.message } });
      else send(500, { error: { code: 'internal', message: 'internal error' } });
    }
  }));
}
