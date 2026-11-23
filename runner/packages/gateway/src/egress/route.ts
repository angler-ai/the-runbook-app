import { isIPv4 } from 'node:net';
import { networkInterfaces } from 'node:os';
import { sandboxIdForClaim } from '@runbook/runner-shared/capability';
import type { Config } from '@runbook/runner-shared/config';
import type { EgressPolicy } from '@runbook/runner-shared/contracts/internal';
import { HttpError, type Route } from '@runbook/runner-shared/http';
import type { CapabilityVerifier } from '../capability.ts';
import type { MaterialStore } from '../material.ts';
import { sandboxListener } from './listener.ts';

export function sandboxRoute(config: Config, verifier: Pick<CapabilityVerifier, 'verifyCapability'>, store: MaterialStore, publicHost: string) {
  const listeners = new Map<string, { listener: Awaited<ReturnType<typeof sandboxListener>>; capability: string }>();
  const creating = new Set<string>();
  let closing = false;
  const kubernetes = process.env.RUNNER_SANDBOX_BACKEND === 'kubernetes';
  const ports = new Set<number>();

  async function authenticate(body: unknown, routeId?: string) {
    const input = body as { capability?: unknown; sandbox_id?: unknown; listen_host?: unknown } | undefined;
    const id = routeId ?? input?.sandbox_id;
    if (typeof input?.capability !== 'string' || typeof id !== 'string') {
      throw new HttpError(400, 'egress.invalid', 'invalid sandbox listener request');
    }
    const claims = await verifier.verifyCapability(input.capability);
    if (id !== sandboxIdForClaim(claims)) throw new HttpError(403, 'egress.sandbox_denied', 'sandbox listener unavailable');
    return { id, claims, capability: input.capability, listenHost: input.listen_host };
  }

  async function policy(capability: string): Promise<EgressPolicy> {
    try {
      // Sandbox traffic drives policy lookups on every request and CONNECT.
      const response = await fetch(`${config.urls.api}/internal/egress/policy`, {
        method: 'POST',
        headers: { authorization: `Bearer ${config.outboundTokens.api}`, 'content-type': 'application/json' },
        body: JSON.stringify({ capability }), redirect: 'error', signal: AbortSignal.timeout(10_000),
      });
      if (!response.ok) throw new Error();
      const result = await response.json() as EgressPolicy;
      if (!Array.isArray(result.layers) || result.layers.length !== 3 ||
          result.layers.some((layer) => !layer || typeof layer !== 'object' || Array.isArray(layer)) ||
          typeof result.destructive !== 'boolean' || !result.credentialLabels ||
          typeof result.credentialLabels !== 'object' || Array.isArray(result.credentialLabels)) throw new Error();
      return result;
    } catch {
      throw new HttpError(503, 'egress.unavailable', 'sandbox listener unavailable');
    }
  }

  const details = ({ proxy_url, listener_secret, ca_pem }: Awaited<ReturnType<typeof sandboxListener>>) =>
    ({ proxy_url, listener_secret, ca_pem });

  const route: Route = {
    method: 'POST', path: '/internal/sandboxes', callers: ['sandbox-ctl'],
    async handle({ body }) {
      const { id, claims, capability, listenHost } = await authenticate(body);
      if (creating.has(id) || listeners.has(id) || closing) {
        throw new HttpError(409, 'egress.listener_exists', 'sandbox listener unavailable');
      }
      creating.add(id);
      try {
        await policy(capability);
        if (closing || claims.exp * 1000 <= Date.now()) throw new Error();
        // A Docker listener is bound to its dedicated bridge address. The
        // control-plane listener must bind a different, explicit interface.
        if (listenHost !== undefined && (typeof listenHost !== 'string' || !isIPv4(listenHost) ||
            !isIPv4(config.listen.host) || config.listen.host === '0.0.0.0' || config.listen.host === listenHost ||
            !Object.values(networkInterfaces()).flat().some((address) => address?.address === listenHost && !address.internal))) {
          throw new Error();
        }
        const entry = { capability };
        const options = {
          claims, store, layers: [], policy: () => policy(entry.capability),
          listenHost: typeof listenHost === 'string' ? listenHost : config.listen.host,
          publicHost: typeof listenHost === 'string' ? listenHost : publicHost,
          onClose: () => { listeners.delete(id); },
        };
        let listener: Awaited<ReturnType<typeof sandboxListener>> | undefined;
        if (!kubernetes) listener = await sandboxListener(options);
        else {
          if (listenHost !== undefined || !isIPv4(publicHost)) throw new Error();
          for (let port = 20000; port <= 29999; port++) {
            if (ports.has(port)) continue;
            ports.add(port);
            try {
              listener = await sandboxListener({ ...options, port, onClose: () => {
                ports.delete(port);
                options.onClose();
              } });
              break;
            } catch (error) {
              ports.delete(port);
              if ((error as NodeJS.ErrnoException).code !== 'EADDRINUSE') throw error;
            }
          }
        }
        if (!listener) throw new Error();
        if (closing || claims.exp * 1000 <= Date.now()) {
          listener.close();
          throw new Error();
        }
        listeners.set(id, Object.assign(entry, { listener }));
        return { status: 201, body: details(listener) };
      } catch {
        throw new HttpError(503, 'egress.unavailable', 'sandbox listener unavailable');
      } finally {
        creating.delete(id);
      }
    },
  };

  const routes: Route[] = [route, {
    method: 'POST', path: '/internal/sandboxes/{sandbox_id}/check', callers: ['sandbox-ctl'],
    async handle({ body, params }) {
      const { id } = await authenticate(body, params.sandbox_id);
      return { status: listeners.has(id) ? 204 : 410 };
    },
  }, {
    method: 'POST', path: '/internal/sandboxes/{sandbox_id}/renew', callers: ['sandbox-ctl'],
    async handle({ body, params }) {
      const { id, claims, capability } = await authenticate(body, params.sandbox_id);
      const entry = listeners.get(id);
      if (!entry) return { status: 404 };
      await policy(capability);
      if (!entry.listener.renew(claims)) return { status: 409 };
      entry.capability = capability;
      return { body: details(entry.listener) };
    },
  }, {
    method: 'DELETE', path: '/internal/sandboxes/{sandbox_id}', callers: ['sandbox-ctl'],
    async handle({ body, params }) {
      const { id, claims } = await authenticate(body, params.sandbox_id);
      const entry = listeners.get(id);
      entry?.listener.close();
      store.endClaim(claims);
      return { status: entry ? 204 : 404 };
    },
  }];

  return {
    routes,
    hasSandbox: (id: string) => listeners.has(id),
    close() {
      closing = true;
      for (const { listener } of listeners.values()) listener.close();
      listeners.clear();
    },
  };
}
