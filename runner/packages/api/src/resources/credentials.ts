import type pg from 'pg';
import type { Config } from '@runbook/runner-shared/config';
import type { IngestKeyResponse, SealResponse, TestResponse } from '@runbook/runner-shared/contracts/internal';
import { CREDENTIAL_TYPES, type CredentialType } from '@runbook/runner-shared/credential-types';
import { HttpError } from '@runbook/runner-shared/http';
import { parseSealedBlob } from '@runbook/runner-shared/seal-client';
import { credentialInput, exposedCredential } from '@runbook/runner-vault/credential-metadata';
import { authorize } from '../authorize.ts';
import { createProposal } from './proposals.ts';
import type { PublicRequest } from '../server.ts';

function invalid(): never {
  throw new HttpError(400, 'credential.invalid', 'invalid credential configuration');
}

export { credentialConfiguration } from '@runbook/runner-vault/credential-metadata';

export function credentialState(current: { binding_hash: Buffer | null; status: string }, nextHash: Buffer): string {
  if (current.status === 'revoked') return 'revoked';
  return current.binding_hash?.equals(nextHash) ? current.status : 'needs_secret';
}

type CredentialRow = {
  id: string;
  name: string;
  type: string;
  config: unknown;
  groups: unknown;
  sharing: unknown;
  label: string;
  status: string;
  binding_hash: Buffer | null;
};

export function credentialResources(pool: pg.Pool, dependencies: Config, sync = async () => {}) {
  async function vault<T>(path: string, payload?: unknown, assertion?: NonNullable<PublicRequest['assertion']>): Promise<T> {
    const response = await fetch(`${dependencies.urls.vault}${path}`, {
      method: assertion || payload !== undefined ? 'POST' : 'GET',
      headers: { authorization: `Bearer ${dependencies.outboundTokens.vault}`, 'content-type': 'application/json',
        ...(assertion && { 'runbook-assertion': assertion.token }) },
      ...(assertion ? { body: new Uint8Array(assertion.rawBody) } :
        payload === undefined ? {} : { body: JSON.stringify(payload) }),
      redirect: 'error', signal: AbortSignal.timeout(10_000),
    });
    if (!response.ok) {
      if (response.status === 409) {
        const failure = await response.json().catch(() => null) as { error?: { code?: string } } | null;
        if (failure?.error?.code === 'credential.revoked') {
          throw new HttpError(409, 'credential.revoked', 'the credential is revoked');
        }
      }
      const status = [400, 401, 403, 404, 409, 410].includes(response.status) ? response.status : 502;
      throw new HttpError(status, 'credential.vault_unavailable', 'credential operation refused');
    }
    return response.status === 204 ? undefined as T : await response.json() as T;
  }

  const handle = async ({ method, claims, body, assertion }: PublicRequest, segments: string[]) => {
    const identifier = segments[4];
    const operation = segments[5];
    const write = method !== 'GET';
    const resource = { kind: 'org' } as const;
    const isAdmin = claims.org_role === 'owner' || claims.org_role === 'admin';
    const revoking = method === 'POST' && operation === 'revoke';
    const action = revoking ? 'operate' : !write ? 'read' : operation === 'secrets' ? 'secret' : 'widen';
    const access = authorize(claims, action, resource);
    // A project admin can revoke a credential owned by their project.
    // Check its scope under the same lock as the revocation.
    const checkProject = revoking && claims.scopes.includes('runner:admin') &&
      Object.values(claims.project_roles).includes('admin');
    if (access === 'deny' && !checkProject) throw new HttpError(403, 'auth.denied', 'runner resource unavailable');
    if (access === 'proposal') {
      if (operation || (method !== 'POST' && method !== 'PATCH')) {
        throw new HttpError(403, 'auth.denied', 'runner resource unavailable');
      }
      const change = credentialInput(body);
      if (change.label !== undefined) throw new HttpError(403, 'auth.denied', 'credential labels must be set directly by an administrator');
      return { status: 202, body: await createProposal(pool, claims, {
        kind: 'credential.change', targetType: 'credential', targetId: identifier ?? null, diff: change,
      }) };
    }

    if (segments[3] === 'ingest-key') {
      if (!isAdmin || claims.principal !== 'human') throw new HttpError(403, 'auth.denied', 'runner resource unavailable');
      return { body: await vault<IngestKeyResponse>('/internal/ingest-key') };
    }
    if (method === 'GET') {
      const projects = Object.keys(claims.project_roles);
      const result = await pool.query<CredentialRow>(
        `SELECT ${exposedCredential} FROM credentials WHERE org_id = $1 AND deleted_at IS NULL AND ($2::text IS NULL OR id = $2)
           AND ($3 OR sharing->>'projects' = 'all' OR (sharing->'projects') ?| $4::text[])
           ORDER BY name LIMIT 1000`,
        [claims.org_id, identifier ?? null, isAdmin, projects],
      );
      const rows = isAdmin ? result.rows : result.rows.flatMap((row) => {
        const shared = (row.sharing as { projects?: unknown } | null)?.projects;
        const visible = projects.filter((project) => shared === 'all' || (Array.isArray(shared) && shared.includes(project)));
        return visible.length ? [{ ...row, sharing: { projects: visible } }] : [];
      });
      return identifier ? { status: rows.length ? 200 : 404, body: rows[0] } : { body: { items: rows } };
    }

    if (identifier && (revoking || (method === 'DELETE' && !operation))) {
      const destroy = method === 'DELETE';
      const { rows: [credential] } = await pool.query<{ project_id: string | null }>(
        'SELECT project_id FROM credentials WHERE id = $1 AND org_id = $2 AND deleted_at IS NULL',
        [identifier, claims.org_id],
      );
      if (!credential) throw new HttpError(404, 'credential.not_found', 'credential unavailable');
      if (access !== 'allow' && (!credential.project_id ||
          authorize(claims, 'operate', { kind: 'agent', projectId: credential.project_id }) !== 'allow')) {
        throw new HttpError(403, 'auth.denied', 'runner resource unavailable');
      }
      if (!assertion) throw new HttpError(401, 'assertion.invalid', 'invalid runner assertion');
      try {
        await vault<void>(`/internal/credentials/${identifier}/${destroy ? 'destroy' : 'revoke'}`, undefined, assertion);
        return { status: 204 };
      } finally {
        await sync();
      }
    }

    const input = body as Record<string, unknown> | undefined;
    if (identifier && operation === 'test') {
      if (!assertion) throw new HttpError(401, 'assertion.invalid', 'invalid runner assertion');
      const credential = await pool.query('SELECT 1 FROM credentials WHERE id = $1 AND org_id = $2', [identifier, claims.org_id]);
      if (!credential.rowCount) throw new HttpError(404, 'credential.not_found', 'credential unavailable');
      return { body: await vault<TestResponse>(`/internal/test/${identifier}`, undefined, assertion) };
    }
    if (identifier && operation === 'secrets') {
      if (!assertion) throw new HttpError(401, 'assertion.invalid', 'invalid runner assertion');
      if (!input || typeof input.blob !== 'string') invalid();
      const field = segments[6];
      try {
        const sealed = parseSealedBlob(input.blob);
        if (sealed.org_id !== claims.org_id || sealed.credential_id !== identifier || sealed.field !== field) invalid();
      } catch {
        invalid();
      }
      const { rows: [credential] } = await pool.query<{ type: CredentialType }>(
        'SELECT type FROM credentials WHERE id = $1 AND org_id = $2 AND status <> $3',
        [identifier, claims.org_id, 'revoked'],
      );
      const fields = credential && CREDENTIAL_TYPES[credential.type]?.secretFields.map(({ name }) => name);
      if (!credential || !field || !fields) invalid();
      if (!fields.includes(field)) {
        // Name the valid fields; echo the requested one only when it is a plain field name.
        const named = /^[a-z][a-z0-9_]{0,63}$/.test(field) ? ` "${field}"` : ' with that name';
        throw new HttpError(400, 'credential.invalid', `${credential.type} credentials have no secret field${named}; ` +
          `use ${fields.map((name) => `"${name}"`).join(' or ')}`);
      }
      const result = await vault<SealResponse>(`/internal/seal/${identifier}/${field}`, undefined, assertion);
      if (result.credential_id !== identifier || result.field !== field || !Number.isSafeInteger(result.version)) {
        throw new HttpError(502, 'credential.vault_unavailable', 'credential operation refused');
      }
      return { body: result };
    }
    if (operation || (method !== 'POST' && method !== 'PATCH')) {
      throw new HttpError(501, 'route.unavailable', 'runner route unavailable');
    }
    if (!assertion) throw new HttpError(401, 'assertion.invalid', 'invalid runner assertion');
    const row = await vault<CredentialRow>(identifier ? `/internal/credential-metadata/${identifier}` : '/internal/credential-metadata',
      undefined, assertion);
    await sync();
    return { status: identifier ? 200 : 201, body: row };
  };
  return Object.assign(handle, {
    async approve(request: PublicRequest, id: string) {
      if (authorize(request.claims, 'approve', { kind: 'org' }) !== 'allow') {
        throw new HttpError(403, 'auth.denied', 'runner resource unavailable');
      }
      if (!request.assertion) throw new HttpError(401, 'assertion.invalid', 'invalid runner assertion');
      await vault<void>(`/internal/credential-proposals/${id}/approve`, undefined, request.assertion);
      await sync();
      return { status: 204 };
    },
  });
}
