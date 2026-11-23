import { randomUUID } from 'node:crypto';
import { isDeepStrictEqual } from 'node:util';
import type pg from 'pg';
import { bindingHash } from '@runbook/runner-shared/binding';
import type { UserAssertionClaims } from '@runbook/runner-shared/contracts/core';
import { CREDENTIAL_TYPES, validateCredentialConfig, type CredentialType } from '@runbook/runner-shared/credential-types';
import { withTransaction } from '@runbook/runner-shared/db';
import { HttpError } from '@runbook/runner-shared/http';
import { findSecretShapedValues } from '@runbook/runner-shared/secret-shape';
import { audit } from './audit.ts';
import type { OAuthAccessService } from './oauth/access.ts';
import { record } from './types/common.ts';
import { canonicalGroups, credentialBinding } from './types/index.ts';

function invalid(): never {
  throw new HttpError(400, 'credential.invalid', 'invalid credential configuration');
}

export function credentialInput(body: unknown): Record<string, unknown> {
  if (!body || typeof body !== 'object' || Array.isArray(body)) invalid();
  const input = body as Record<string, unknown>;
  if (Object.keys(input).some((key) => !['name', 'type', 'config', 'groups', 'sharing', 'label'].includes(key)) ||
      findSecretShapedValues(input).some((finding) =>
        !(finding.path === 'config.client_id' && finding.reason === 'high-entropy'))) invalid();
  return input;
}

export function credentialConfiguration(type: string, raw: unknown, groups: unknown, previous?: unknown) {
  if (!Object.hasOwn(CREDENTIAL_TYPES, type) || !raw || typeof raw !== 'object' || Array.isArray(raw)) invalid();
  const manifest = CREDENTIAL_TYPES[type as CredentialType];
  let config = record(raw);
  if (type === 'mcp-oauth') {
    const old = previous === undefined ? undefined : record(previous);
    if (old?.issuer && config !== old) {
      // The provider endpoints and client identity come from discovery and confirmation.
      for (const key of Object.keys(config)) {
        if (!['scopes', 'extra_parameters'].includes(key) && !isDeepStrictEqual(old[key], config[key])) invalid();
      }
      config = { ...old, ...config };
    } else if (!old?.issuer && Object.keys(config).some((key) => !manifest.configFields.some(({ name }) => name === key))) {
      invalid();
    }
    if (!config.issuer && config.registration_mode === 'dynamic' &&
        (config.client_id !== undefined || config.token_endpoint_auth_method !== undefined)) invalid();
  } else if (Object.keys(config).some((key) =>
    !(key === 'secret_source' && manifest.secretSources) && !manifest.configFields.some(({ name }) => name === key)) ||
      validateCredentialConfig(manifest.type, config).length) invalid();
  if (config.test && (typeof config.test !== 'object' || Array.isArray(config.test) ||
      Object.keys(config.test).some((key) => key !== 'method' && key !== 'path'))) invalid();
  try {
    const allowedGroups = canonicalGroups(groups);
    const binding = credentialBinding(type, config, allowedGroups);
    if (binding.secret_source !== undefined) config = { ...config, secret_source: binding.secret_source };
    return { manifest, config, groups: allowedGroups, binding, hash: bindingHash(type, binding) };
  } catch {
    return invalid();
  }
}

export const exposedCredential = `id, name, type, scope, project_id, config, groups, sharing, label, label_initialized, status, delivery, secret_meta, oauth_status,
  (delivery ? 'proxy') AS placeholder_eligible,
  translate(rtrim(encode(binding_hash, 'base64'), '='), '+/', '-_') AS binding_hash`;

export function credentialProjects(sharing: unknown): 'none' | 'all' | string[] {
  const projects = (sharing as { projects?: unknown })?.projects;
  if (projects === 'none' || projects === 'all') return projects;
  if (!Array.isArray(projects) || projects.some((project) => typeof project !== 'string')) invalid();
  return projects as string[];
}

interface CredentialPolicyRow {
  id: string;
  name: string;
  type: string;
  config: unknown;
  groups: unknown;
  sharing: unknown;
  label: string;
  label_initialized: boolean;
  status: string;
  created_by: string | null;
}

function requireCredentialAdmin(claims: UserAssertionClaims) {
  if (claims.principal !== 'human' || !claims.scopes.includes('runner:admin') ||
      (claims.org_role !== 'owner' && claims.org_role !== 'admin')) {
    throw new HttpError(403, 'auth.denied', 'credential operation refused');
  }
}

export async function writeCredentialMetadata(pool: pg.Pool, claims: UserAssertionClaims, body: unknown, id?: string, oauth?: OAuthAccessService) {
  requireCredentialAdmin(claims);
  return withTransaction(pool, (client) => writeMetadata(client, claims, body, id, oauth));
}

async function writeMetadata(client: pg.PoolClient, claims: UserAssertionClaims, body: unknown, id?: string, oauth?: OAuthAccessService) {
  const change = credentialInput(body);
  try {
    if (!id) {
      if (typeof change.name !== 'string') invalid();
      await client.query('SELECT pg_advisory_xact_lock(hashtextextended($1, 7419011))', [change.name]);
    } else {
      await client.query('SELECT pg_advisory_xact_lock(hashtextextended($1, 7419006))', [id]);
    }
    const { rows: [current] } = await client.query<CredentialPolicyRow>(
      `SELECT * FROM credentials WHERE org_id = $1 AND scope = 'org' AND deleted_at IS NULL
       AND (${id ? 'id' : 'name'}) = $2 FOR UPDATE`, [claims.org_id, id ?? change.name]);
    if (id && !current) throw new HttpError(404, 'credential.not_found', 'credential unavailable');
    if (current?.status === 'revoked') throw new HttpError(409, 'credential.revoked', 'credential is revoked');

    const name = change.name ?? (id ? current?.name : undefined);
    const type = change.type ?? (id ? current?.type : undefined);
    if (typeof name !== 'string' || !name || name.length > 128 || typeof type !== 'string' ||
        (id && current?.type !== type)) invalid();
    const configured = credentialConfiguration(type, change.config ?? (id ? current?.config : undefined),
      change.groups ?? (id ? current?.groups : undefined) ?? 'all', id ? current?.config : undefined);
    const label = change.label ?? (id ? current?.label : undefined) ?? configured.manifest.defaultLabel;
    if (label !== 'read' && label !== 'write' && label !== 'destructive') invalid();
    const sharing = { projects: credentialProjects(change.sharing ?? (id ? current?.sharing : undefined) ?? { projects: 'none' }) };
    const unchanged = current && current.name === name && current.type === type && current.label === label &&
      isDeepStrictEqual(current.config, configured.config) && isDeepStrictEqual(current.groups, configured.groups) &&
      isDeepStrictEqual(current.sharing, sharing);

    if (!id && current && !(unchanged && current.label_initialized && current.created_by === claims.user_id)) {
      throw new HttpError(409, 'credential.name_conflict', 'credential name already exists');
    }

    const credential = current?.id ?? randomUUID();
    if (!current) {
      await client.query(
        `INSERT INTO credentials(id, org_id, name, type, config, binding, binding_hash, sharing, groups, delivery, created_by,
          label, label_initialized) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,true)`,
        [credential, claims.org_id, name, type, JSON.stringify(configured.config), JSON.stringify(configured.binding), configured.hash,
          JSON.stringify(sharing), JSON.stringify(configured.groups), JSON.stringify(configured.manifest.deliveries), claims.user_id, label]);
    } else if (!unchanged || (!current.label_initialized && change.label !== undefined)) {
      const oldHash = type === 'mcp-oauth' ? bindingHash(type, credentialBinding(type, current.config, canonicalGroups(current.groups))) : null;
      const bindingChanged = oldHash && !oldHash.equals(configured.hash) && oauth &&
        await oauth.changeBinding(client, credential, configured, claims.user_id);
      await client.query(
        `UPDATE credentials SET name = $2, config = $3, binding = $4, binding_hash = $5,
          sharing = $6, groups = $7, updated_at = now() WHERE id = $1`,
        [credential, name, JSON.stringify(configured.config), JSON.stringify(configured.binding), configured.hash,
          JSON.stringify(sharing), JSON.stringify(configured.groups)]);
      if (oldHash && !oldHash.equals(configured.hash)) {
        // Discovery confirms a particular draft binding. Don't show a stale
        // candidate after the administrator edits the draft.
        await client.query(`UPDATE credentials SET oauth_status = NULLIF(oauth_status - 'discovery_candidate', '{}'::jsonb) WHERE id = $1`,
          [credential]);
        await client.query(`UPDATE credential_oauth SET candidate = NULL, candidate_hash = NULL, registration_operation = NULL
          WHERE credential_id = $1 AND candidate_hash IS NOT NULL`, [credential]);
      }
      if (bindingChanged) {
        await client.query(`UPDATE credentials SET status = 'reconnect_required',
          oauth_status = COALESCE(oauth_status, '{}') || '{"connect_result":"reconnect_required"}'::jsonb WHERE id = $1`, [credential]);
      }
      if (change.label !== undefined && (current.label !== label || !current.label_initialized)) {
        await client.query('UPDATE credentials SET label = $2, label_initialized = true WHERE id = $1', [credential, label]);
        await audit(client, {
          actor: { kind: 'user', id: claims.user_id }, action: 'credential.label_changed',
          resource_type: 'credential', resource_id: credential, group_id: null, runner_id: null, detail: { label },
        });
      }
    }
    const { rows: [saved] } = await client.query<{
      id: string; name: string; binding_hash: string; label: string; status: string; sharing: { projects: string | string[] };
    }>(`SELECT ${exposedCredential} FROM credentials WHERE id = $1`, [credential]);
    if (!unchanged || (!current?.label_initialized && change.label !== undefined)) {
      await audit(client, {
        actor: { kind: 'user', id: claims.user_id }, action: current ? 'credential.updated' : 'credential.created',
        resource_type: 'credential', resource_id: credential, group_id: null, runner_id: null, detail: { status: saved?.status },
      });
    }
    if (!saved) throw new Error('credential write did not return a row');
    return saved;
  } catch (error) {
    if ((error as { code?: string } | null)?.code === '23505') {
      throw new HttpError(409, 'credential.name_conflict', 'credential name already exists');
    }
    throw error;
  }
}

export async function approveCredentialProposal(pool: pg.Pool, claims: UserAssertionClaims, id: string, oauth?: OAuthAccessService): Promise<number> {
  requireCredentialAdmin(claims);
  return withTransaction(pool, async (client) => {
    const { rows: [proposal] } = await client.query<{
      status: string; expires_at: Date; target_id: string | null; diff: unknown;
    }>("SELECT status, expires_at, target_id, diff FROM proposals WHERE id = $1 AND kind = 'credential.change' FOR UPDATE", [id]);
    if (!proposal) throw new HttpError(404, 'proposal.not_found', 'proposal unavailable');
    if (proposal.status === 'expired') return 410;
    if (proposal.status !== 'open') throw new HttpError(409, 'proposal.closed', 'proposal already decided');
    if (proposal.expires_at.getTime() <= Date.now()) {
      await client.query("UPDATE proposals SET status = 'expired' WHERE id = $1", [id]);
      return 410;
    }
    const change = credentialInput(proposal.diff);
    if (change.label !== undefined) throw new HttpError(403, 'auth.denied', 'credential labels must be set directly by an administrator');
    await writeMetadata(client, claims, change, proposal.target_id ?? undefined, oauth);
    await client.query("UPDATE proposals SET status = 'approved', decided_at = now(), decided_by = $2 WHERE id = $1",
      [id, claims.user_id]);
    await audit(client, {
      actor: { kind: 'user', id: claims.user_id }, action: 'proposal.approved',
      resource_type: 'proposal', resource_id: id, group_id: null, runner_id: null, detail: {},
    });
    return 204;
  });
}
