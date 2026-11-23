import type pg from 'pg';
import { GoogleAuth, type AuthClient } from 'google-auth-library';
import type { Delivery, GrantClaims } from '@runbook/runner-shared/contracts/core';
import { isCredentialPlaceholder, isVisibleAsciiToken } from '@runbook/runner-shared/credential-source';
import { vertexBinding } from '@runbook/runner-shared/vertex-binding';
import { VaultError } from './errors.ts';
import { notAllowed } from './policy.ts';

export const ATTACHED_VERTEX_FIELD = 'attached_vertex';

const SCOPE = 'https://www.googleapis.com/auth/cloud-platform';
const LIFETIME_SECONDS = 600;

interface VertexTarget {
  project: string;
  locations: readonly string[];
  service_account: string;
}

export function vertexTargets(raw: string | undefined): VertexTarget[] {
  if (raw === undefined) return [];
  const invalid = () => new Error('RUNNER_VERTEX_TARGETS must be a JSON array of entries with project, locations and service_account');
  let entries: unknown;
  try {
    entries = JSON.parse(raw);
  } catch {
    throw invalid();
  }
  if (!Array.isArray(entries)) throw invalid();
  const projects = new Set<string>();
  for (const entry of entries as (Partial<VertexTarget> | null)[]) {
    if (!entry || typeof entry.service_account !== 'string' ||
        !/^[a-z][a-z0-9-]{4,28}[a-z0-9]@[a-z][a-z0-9-]{4,28}[a-z0-9]\.iam\.gserviceaccount\.com$/.test(entry.service_account) ||
        !Array.isArray(entry.locations) || entry.locations.length === 0 ||
        !entry.locations.every((location) => vertexBinding('vertex', entry.project, location)) ||
        typeof entry.project !== 'string' || projects.has(entry.project)) throw invalid();
    projects.add(entry.project);
  }
  return entries as VertexTarget[];
}

interface VertexIdentityOptions {
  targets?: readonly VertexTarget[];
  auth?: { getClient(): Promise<AuthClient> };
  iamOrigin?: string;
  now?: () => Date;
}

const unavailable = () => new VaultError(503, 'vertex.adc_unavailable', 'the vault could not mint a usable Vertex access token');
const iamOrigin = 'https://iamcredentials.googleapis.com';

export class VertexIdentity {
  private readonly options: VertexIdentityOptions;
  private readonly auth: NonNullable<VertexIdentityOptions['auth']>;
  private readonly now: () => Date;

  constructor(options: VertexIdentityOptions = {}) {
    this.options = options;
    this.auth = options.auth ?? new GoogleAuth({ scopes: SCOPE });
    this.now = options.now ?? (() => new Date());
  }

  assertTarget(type: unknown, project: unknown, region: unknown) {
    const binding = vertexBinding(type, project, region);
    const target = binding && this.options.targets?.find((entry) =>
      entry.project === binding.project_id && entry.locations.includes(binding.region));
    if (!binding || !target) throw notAllowed();
    return { binding, serviceAccount: target.service_account };
  }

  async access(serviceAccount: string, grantExpiry: number) {
    const startedAt = this.now().getTime();
    if (grantExpiry <= startedAt) throw new VaultError(403, 'grant.expired', 'the release grant expired');

    let data: { accessToken?: unknown; expireTime?: unknown } | null;
    try {
      const client = await this.auth.getClient();
      const url = `${this.options.iamOrigin ?? iamOrigin}/v1/projects/-/serviceAccounts/${serviceAccount}:generateAccessToken`;
      ({ data } = await client.request<NonNullable<typeof data>>({
        url, method: 'POST', maxRedirects: 0, timeout: 10_000, retry: false, responseType: 'json',
        data: { scope: [SCOPE], lifetime: `${LIFETIME_SECONDS}s` },
      }));
    } catch (error) {
      // Google auth errors can include headers, tokens, and raw responses.
      const failure = error as { code?: unknown; response?: { status?: number; data?: { error?: unknown } } } | null;
      const status = failure?.response?.status;
      if (status === 401 || status === 403 || failure?.code === 401 || failure?.code === 403 ||
          ['invalid_grant', 'unauthorized_client', 'access_denied', 'invalid_scope'].includes(String(failure?.response?.data?.error))) {
        throw new VaultError(403, 'vertex.adc_forbidden', 'Google refused Vertex service account impersonation');
      }
      if (status === 429 || (status !== undefined && status >= 500) ||
          ['ETIMEDOUT', 'ECONNRESET', 'EAI_AGAIN', 'ENOTFOUND'].includes(String(failure?.code))) {
        throw new VaultError(503, 'vertex.adc_retry', 'Google authentication is temporarily unavailable');
      }
      throw unavailable();
    }
    const token = data?.accessToken;
    const expiresAt = typeof data?.expireTime === 'string' ? Date.parse(data.expireTime) : NaN;
    const now = this.now().getTime();
    if (!isVisibleAsciiToken(token) || isCredentialPlaceholder(token) ||
        !Number.isFinite(expiresAt) || expiresAt <= now || expiresAt > startedAt + 900_000) throw unavailable();
    if (grantExpiry <= now) throw new VaultError(403, 'grant.expired', 'the release grant expired');
    return { material: { fields: { access_token: token } }, expiresAt: Math.min(grantExpiry, expiresAt) };
  }
}

type VertexPolicyClaims = Pick<GrantClaims, 'agent_version' | 'agent_id' | 'project_id' | 'group_id' | 'reach' | 'model_id'>
  & Partial<Pick<GrantClaims, 'model_profile' | 'connections'>>;

export async function checkVertexPolicy(
  identity: VertexIdentity | undefined,
  db: pg.Pool | pg.PoolClient,
  claims: VertexPolicyClaims,
  bindingId: string,
  delivery: Delivery,
) {
  if (!identity || delivery !== 'model' || (claims.reach === 'read' && (!claims.model_id || !claims.connections?.includes(bindingId)))) throw notAllowed();
  const { rows: [row] } = await db.query<{ type: string; project_id: string; region: string; endpoint: string }>(
    `SELECT v.type, v.project_id, v.region, v.endpoint
     FROM agent_versions av JOIN agents a ON a.id = av.agent_id
     JOIN profiles p ON av.config->'profiles' ? p.id
     JOIN providers v ON v.id = p.provider_id
     WHERE av.id = $1 AND a.id = $2 AND a.project_id = $3 AND a.group_id = $4 AND a.status = 'active'
       AND v.attached_binding_id = $5 AND v.auth = 'attached' AND v.credential_id IS NULL
       AND ($6::text IS NULL OR p.name = $6)
       AND (p.sharing->>'projects' = 'all' OR p.sharing->'projects' ? $3)
       AND (v.sharing->>'projects' = 'all' OR v.sharing->'projects' ? $3)
       AND ($7::text IS NULL OR p.model = $7) LIMIT 1`,
    [claims.agent_version, claims.agent_id, claims.project_id, claims.group_id, bindingId, claims.model_profile,
      claims.reach === 'read' ? claims.model_id : null],
  );
  const target = identity.assertTarget(row?.type, row?.project_id, row?.region);
  if (target.binding.endpoint !== row?.endpoint) throw notAllowed();
  return target;
}
