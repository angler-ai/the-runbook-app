import { DELIVERIES } from '@runbook/runner-shared/contracts/core';
import type { ClaimFence, MaterialPush } from '@runbook/runner-shared/contracts/internal';
import { createJsonServer, HttpError, type Route, type ServerOptions } from '@runbook/runner-shared/http';
import { MaterialStore } from './material.ts';

const id = (value: unknown): value is string => typeof value === 'string' && /^[A-Za-z0-9_.:-]{1,128}$/.test(value);
const object = (value: unknown): boolean => value !== null && typeof value === 'object' && !Array.isArray(value);

export function createGatewayServer(options: Omit<ServerOptions, 'routes'>, store = new MaterialStore(), extraRoutes: readonly Route[] = []) {
  const server = createJsonServer({
    ...options,
    routes: [
      {
        method: 'POST', path: '/internal/material', callers: ['vault'],
        async handle({ body }) {
          const release = body as MaterialPush | undefined;
          if (release && id(release.release_id) && id(release.step_run_id) && id(release.credential_id) &&
              DELIVERIES.includes(release.delivery) && object(release.binding) &&
              object(release.material?.fields) && Object.values(release.material.fields).length > 0 &&
              Object.values(release.material.fields).every((value) => typeof value === 'string' && value.length > 0) &&
              typeof release.expires_at === 'string' &&
              (release.step_run_id === `test:${release.release_id}`
                ? release.claim_id === undefined && release.generation === undefined && release.runner_id === undefined
                : id(release.claim_id) && id(release.runner_id) && Number.isSafeInteger(release.generation))) {
            try {
              store.put(release);
              return { status: 204 };
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
      ...extraRoutes,
    ],
  });
  server.once('close', () => store.clear());
  return server;
}
