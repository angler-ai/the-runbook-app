import { randomUUID } from 'node:crypto';
import { isCredentialPlaceholder, isVisibleAsciiToken } from '@runbook/runner-shared/credential-source';
import { DELIVERIES } from '@runbook/runner-shared/contracts/core';
import type { ClaimFence, MaterialPush } from '@runbook/runner-shared/contracts/internal';
import { createJsonServer, HttpError, type Route, type ServerOptions } from '@runbook/runner-shared/http';
import { addOAuthTunnel, oauthDestinationRoute } from './egress/oauth-tunnel.ts';
import { MaterialStore } from './material.ts';

const id = (value: unknown): value is string => typeof value === 'string' && /^[A-Za-z0-9_.:-]{1,128}$/.test(value);
const object = (value: unknown): boolean => value !== null && typeof value === 'object' && !Array.isArray(value);

function validMaterial(release: MaterialPush): boolean {
  const fields = release.material?.fields;
  if (!fields || !object(fields)) return false;
  const oauth = release.material.mcp_oauth;
  if (!oauth) return Object.values(fields).length > 0 &&
    Object.values(fields).every((value) => typeof value === 'string' && value.length > 0 && !isCredentialPlaceholder(value));
  return release.delivery === 'mcp' && Object.keys(fields).length === 0 &&
    Object.keys(oauth).length === 3 && isVisibleAsciiToken(oauth.access_token) &&
    !isCredentialPlaceholder(oauth.access_token) &&
    Number.isSafeInteger(oauth.generation) && oauth.generation > 0 &&
    Number.isSafeInteger(oauth.token_version) && oauth.token_version > 0;
}

export function createGatewayServer(options: Omit<ServerOptions, 'routes'>, store = new MaterialStore(), extraRoutes: readonly Route[] = []) {
  const epoch = { 'runbook-gateway-epoch': randomUUID() };
  const server = createJsonServer({
    ...options,
    routes: [
      {
        method: 'GET', path: '/internal/material/epoch', callers: ['vault'],
        async handle() { return { status: 204, headers: epoch }; },
      },
      {
        method: 'POST', path: '/internal/material', callers: ['vault'],
        async handle({ body }) {
          const release = body as MaterialPush | undefined;
          if (release && id(release.release_id) && id(release.step_run_id) && id(release.credential_id) &&
              DELIVERIES.includes(release.delivery) && object(release.binding) &&
              validMaterial(release) &&
              typeof release.expires_at === 'string' &&
              (release.step_run_id === `test:${release.release_id}`
                ? release.claim_id === undefined && release.generation === undefined && release.runner_id === undefined
                : id(release.claim_id) && id(release.runner_id) && Number.isSafeInteger(release.generation))) {
            try {
              store.put(release);
              return { status: 204, headers: epoch };
            } catch {
              // Invalid or already expired release.
            }
          }
          throw new HttpError(400, 'material.invalid', 'invalid material push');
        },
      },
      {
        method: 'POST', path: '/internal/material/end-claim', callers: ['vault'],
        async handle({ body }) {
          const claim = body as ClaimFence | undefined;
          if (!claim || !id(claim.sub) || !id(claim.claim_id) || !id(claim.runner_id) ||
              !Number.isSafeInteger(claim.generation) || claim.generation < 0) {
            throw new HttpError(400, 'material.invalid', 'invalid claim fence');
          }
          store.endClaim(claim);
          return { status: 204 };
        },
      },
      {
        method: 'DELETE', path: '/internal/material/{id}', callers: ['vault'],
        async handle({ params }) {
          return { status: store.delete(params.id ?? '') ? 204 : 404 };
        },
      },
      {
        method: 'DELETE', path: '/internal/material/credentials/{credential_id}', callers: ['vault'],
        async handle({ params }) {
          if (!id(params.credential_id)) throw new HttpError(400, 'material.invalid', 'invalid credential ID');
          store.revokeCredential(params.credential_id);
          return { status: 204 };
        },
      },
      {
        method: 'POST', path: '/internal/material/credentials/{credential_id}/oauth-fence', callers: ['vault'],
        async handle({ params, body }) {
          const generation = (body as { generation?: number } | null)?.generation;
          if (!id(params.credential_id) || typeof generation !== 'number' || !Number.isSafeInteger(generation) || generation < 1) {
            throw new HttpError(400, 'material.invalid', 'invalid OAuth fence');
          }
          store.fenceOAuth(params.credential_id, generation);
          return { status: 204 };
        },
      },
      {
        method: 'POST', path: '/internal/material/credentials/{credential_id}/oauth-use', callers: ['vault'],
        async handle({ params, body }) {
          const use = body as { generation?: number; token_version?: number } | undefined;
          if (!id(params.credential_id) || !use?.generation || !Number.isSafeInteger(use.generation) ||
              !use.token_version || !Number.isSafeInteger(use.token_version)) {
            throw new HttpError(400, 'material.invalid', 'invalid OAuth version');
          }
          return { body: { last_success_ms: store.lastOAuthUse(params.credential_id, use.generation, use.token_version) } };
        },
      },
      oauthDestinationRoute,
      ...extraRoutes,
    ],
  });
  addOAuthTunnel(server, options.inboundTokens.vault);
  server.once('close', () => store.clear());
  return server;
}
