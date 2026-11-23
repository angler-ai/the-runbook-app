import { inSpan, requestContext, serverKind } from '@runbook/runner-shared/telemetry';
import type http from 'node:http';
import { createConnection } from 'node:net';
import { HttpError, sameToken, type Route } from '@runbook/runner-shared/http';
import { connectHost, pinnedAddress } from './rules.ts';

const deadlineMs = 20_000;

export const oauthDestinationRoute: Route = {
  method: 'POST', path: '/internal/oauth/destination', callers: ['vault'],
  async handle({ body }) {
    const authority = (body as { authority?: unknown } | undefined)?.authority;
    if (typeof authority !== 'string') throw new HttpError(400, 'egress.denied', 'egress denied');
    await pinnedAddress(connectHost(authority));
    return { status: 204 };
  },
};

export function addOAuthTunnel(server: http.Server, vaultToken: string | undefined): void {
  server.on('connect', (request, downstream, head) => {
    downstream.on('error', () => downstream.destroy());
    const refuse = (status: string) => {
      downstream.end(`HTTP/1.1 ${status}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n`);
    };
    const auth = request.headers['proxy-authorization'];
    if (!vaultToken || !auth || !sameToken(auth, `Bearer ${vaultToken}`)) {
      refuse('407 Proxy Authentication Required');
      return;
    }
    downstream.pause();
    const deadline = setTimeout(() => downstream.destroy(), deadlineMs);
    deadline.unref();
    downstream.once('close', () => clearTimeout(deadline));
    void inSpan('oauth.connect', async () => {
      let target;
      try {
        target = await pinnedAddress(connectHost(request.url ?? ''));
      } catch {
        refuse('403 Forbidden');
        return;
      }
      if (downstream.destroyed) return;
      const upstream = createConnection({ host: target.address, port: 443, family: target.family });
      downstream.once('close', () => upstream.destroy());
      upstream.once('error', () => {
        if (upstream.connecting || !established) refuse('502 Bad Gateway');
        else downstream.destroy();
      });
      let established = false;
      await new Promise<void>((resolve) => {
        upstream.once('error', () => resolve());
        upstream.once('close', () => resolve());
        upstream.once('connect', () => {
          established = true;
          downstream.write('HTTP/1.1 200 Connection Established\r\n\r\n');
          if (head.length) upstream.write(head);
          downstream.pipe(upstream).pipe(downstream);
          resolve();
        });
      });
    }, serverKind, requestContext(request.headers)).catch(() => downstream.destroy());
  });
}
