// The vault's internal HTTP API (plan "Runner internal routes"): reached
// only from runner-api (seal, ingest key, test) and runner-worker
// (capability, release) on the internal network, each with its own token.
import type http from 'node:http';
import type { RunnerProcess } from '@runbook/runner-shared/contracts/internal';
import { createJsonServer, HttpError, type Route, type RouteContext } from '@runbook/runner-shared/http';
import { VaultError } from './errors.ts';
import { approveCredentialProposal, writeCredentialMetadata } from './credential-metadata.ts';
import { handleSeal } from './routes/seal.ts';
import type { Vault } from './vault.ts';

export interface VaultServerOptions {
  inboundTokens: Partial<Record<RunnerProcess, string>>;
  workerTokens?: Record<string, string>;
  soleRunner?: string;
  /** Extra routes (capability, release, test) from the release service. */
  routes?: Route[];
}

export function vaultRoutes(vault: Vault, soleRunner?: string): Route[] {
  function workerRunner(ctx: RouteContext): string {
    const runner = ctx.runnerId ?? soleRunner;
    if (!runner) throw new HttpError(403, 'release.runner_denied', 'worker is not assigned to a runner');
    return runner;
  }
  async function asserted(ctx: RouteContext, target: string, method = 'POST') {
    const token = ctx.request?.headers['runbook-assertion'];
    if (!vault.assertions || typeof token !== 'string' || !ctx.rawBody) {
      throw new HttpError(401, 'assertion.invalid', 'invalid runner assertion');
    }
    return vault.assertions.verifyAssertion({ token, method, target, body: ctx.rawBody });
  }
  async function credentialOperator(ctx: RouteContext, target: string) {
    const claims = await asserted(ctx, target);
    if (claims.principal !== 'human' || !claims.scopes.includes('runner:admin') ||
        (claims.org_role !== 'admin' && claims.org_role !== 'owner')) {
      throw new HttpError(403, 'auth.denied', 'credential operation refused');
    }
    return { kind: 'user' as const, id: claims.user_id };
  }
  const releases = vault.releases;
  const releaseRoutes: Route[] = releases
    ? [
        {
          method: 'POST',
          path: '/internal/capability',
          callers: ['worker', 'sandbox-ctl'],
          handle: async (ctx) => ({ body: await releases.capability(ctx.body as never,
            ctx.caller === 'worker' ? workerRunner(ctx) : undefined) }),
        },
        {
          method: 'POST',
          path: '/internal/release',
          callers: ['worker'],
          handle: async (ctx) => ({ body: await releases.release(ctx.body as never, workerRunner(ctx)) }),
        },
        {
          method: 'POST', path: '/internal/oauth/rejected', callers: ['worker'],
          async handle(ctx) {
            await releases.oauthRejected(ctx.body, workerRunner(ctx));
            return { status: 204 };
          },
        },
        {
          method: 'POST',
          path: '/internal/end-claim',
          callers: ['worker'],
          async handle(ctx) {
            await releases.endClaim(ctx.body as never, workerRunner(ctx));
            return { status: 204 };
          },
        },
        {
          method: 'POST',
          path: '/internal/credentials/{credential_id}/revoke',
          callers: ['api'],
          async handle(ctx) {
            const id = ctx.params.credential_id!;
            const claims = await asserted(ctx, `/runner/v1/credentials/${id}/revoke`);
            await releases.revoke(id, false, claims);
            return { status: 204 };
          },
        },
        {
          method: 'POST',
          path: '/internal/credentials/{credential_id}/destroy',
          callers: ['api'],
          async handle(ctx) {
            const id = ctx.params.credential_id!;
            const claims = await asserted(ctx, `/runner/v1/credentials/${id}`, 'DELETE');
            await releases.revoke(id, true, claims);
            return { status: 204 };
          },
        },
        {
          method: 'POST', path: '/internal/mcp/{server_id}/oauth-discover', callers: ['api'],
          async handle(ctx) {
            const id = ctx.params.server_id!;
            await credentialOperator(ctx, `/runner/v1/mcp-servers/${id}/discover`);
            const { rows: [source] } = await vault.pool.query<{ url: string; transport: 'streamable-http' | 'sse'; credential_id?: string }>(
              'SELECT url, transport, credential_id FROM mcp_servers WHERE id = $1 AND deleted_at IS NULL', [id],
            );
            if (!source?.credential_id) throw new HttpError(404, 'mcp.discovery_denied', 'MCP OAuth server unavailable');
            return { body: await releases.discover(source, true) };
          },
        },
        {
          method: 'POST',
          path: '/internal/mcp/discover',
          callers: ['api'],
          handle: async ({ body }) => ({ body: await releases.discover(body as never) }),
        },
        {
          method: 'POST',
          path: '/internal/test/{credential_id}',
          callers: ['api'],
          async handle(ctx) {
            const credential_id = ctx.params.credential_id!;
            const actor = await credentialOperator(ctx, `/runner/v1/credentials/${credential_id}/test`);
            return { body: await releases.test({ credential_id }, actor) };
          },
        },
      ]
    : [];
  const oauthRoutes: Route[] = vault.oauth
    ? ['oauth-discover', 'oauth-confirm', 'connect', 'disconnect', 'callback'].map((operation) => ({
        method: 'POST', path: `/internal/oauth/{credential_id}/${operation}`, callers: ['api'],
        async handle(ctx) {
          const id = ctx.params.credential_id!;
          const claims = await asserted(ctx, operation === 'callback'
            ? `/runner/v1/oauth/callback/${id}` : `/runner/v1/credentials/${id}/${operation}`);
          const oauth = vault.oauth!;
          switch (operation) {
            case 'oauth-discover': return { body: await oauth.discover(id, claims) };
            case 'oauth-confirm': {
              const result = await oauth.confirm(id, claims, ctx.body);
              if (!('connected' in result) || !result.connected || !releases) return { body: result };
              const test = await releases.test({ credential_id: id }, { kind: 'user', id: claims.user_id })
                .catch(() => ({ ok: false, error_class: 'unreachable' }));
              await vault.pool.query(`UPDATE credentials c SET oauth_status = oauth_status || $2::jsonb
                FROM credential_oauth o WHERE c.id = $1 AND c.id = o.credential_id AND o.generation = $3
                  AND c.oauth_status->>'connect_result' = 'connected'`, [id,
                JSON.stringify({ connect_test: test.ok ? 'passed' : test.error_class }), result.generation]);
              return { body: { ...result, test } };
            }
            case 'connect': return { body: await oauth.start(id, claims, ctx.body) };
            case 'disconnect': {
              const result = await oauth.disconnect(id, claims);
              return { status: result.pending_purge ? 202 : 200, body: result };
            }
            default: return { body: await oauth.callback(id, claims, ctx.body) };
          }
        },
      }))
    : [];
  return [
    ...releaseRoutes,
    ...oauthRoutes,
    {
      method: 'POST', path: '/internal/credential-proposals/{proposal_id}/approve', callers: ['api'],
      async handle(ctx) {
        const id = ctx.params.proposal_id!;
        const claims = await asserted(ctx, `/runner/v1/proposals/${id}/approve`);
        return { status: await approveCredentialProposal(vault.pool, claims, id, vault.oauth) };
      },
    },
    {
      method: 'POST', path: '/internal/credential-metadata', callers: ['api'],
      async handle(ctx) {
        const claims = await asserted(ctx, '/runner/v1/credentials');
        if (!vault.installId) throw new HttpError(503, 'credential.vault_unavailable', 'vault installation unavailable');
        const saved = await writeCredentialMetadata(vault.pool, claims, ctx.body);
        return { body: { ...saved, creation_receipt: vault.identity.credentialReceipt(claims.org_id, vault.installId, saved) } };
      },
    },
    {
      method: 'POST', path: '/internal/credential-metadata/{credential_id}', callers: ['api'],
      async handle(ctx) {
        const id = ctx.params.credential_id!;
        const claims = await asserted(ctx, `/runner/v1/credentials/${id}`, 'PATCH');
        return { body: await writeCredentialMetadata(vault.pool, claims, ctx.body, id, vault.oauth) };
      },
    },
    {
      method: 'GET',
      path: '/internal/ingest-key',
      callers: ['api'],
      handle: async () => ({ body: await vault.ingestKey() }),
    },
    {
      method: 'POST',
      path: '/internal/seal/{credential_id}/{field}',
      callers: ['api'],
      async handle(ctx) {
        const credentialId = ctx.params.credential_id!;
        const field = ctx.params.field!;
        const actor = await credentialOperator(ctx, `/runner/v1/credentials/${credentialId}/secrets/${field}`);
        return { body: await handleSeal(vault, ctx.body as never, actor, { credentialId, field }) };
      },
    },
  ];
}

export function createVaultServer(vault: Vault, options: VaultServerOptions): http.Server {
  return createJsonServer({
    routes: [...vaultRoutes(vault, options.soleRunner), ...(options.routes ?? [])],
    inboundTokens: options.inboundTokens,
    workerTokens: options.workerTokens,
    log: vault.log,
    maxBodyBytes: 512 * 1024,
    decisionSupport: true,
    mapError: (e) => (e instanceof VaultError ? new HttpError(e.status, e.code, e.message) : undefined),
  });
}
