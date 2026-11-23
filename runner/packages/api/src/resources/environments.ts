import { randomUUID } from 'node:crypto';
import type pg from 'pg';
import type { Environment, EnvironmentConfig } from '@runbook/runner-shared/contracts/environments';
import { CREDENTIAL_ENV } from '@runbook/runner-shared/credential-source';
import { withTransaction } from '@runbook/runner-shared/db';
import { HttpError } from '@runbook/runner-shared/http';
import { findSecretShapedValues } from '@runbook/runner-shared/secret-shape';
import { authorize } from '../authorize.ts';
import type { PublicRequest } from '../server.ts';
import { validateEgress } from './agents.ts';
import { digestImage, imageHost, registryHost, resolveImage } from './environment-images.ts';

export const sizes = ['small', 'medium', 'large'] as const;
const defaults: EnvironmentConfig = {
  image: null, setup: null, setup_timeout_s: 120, env: {}, credential_env: {},
  size: 'small', timeout_s: 600, egress: null, budgets: {},
};
const variableName = new RegExp(CREDENTIAL_ENV.name_pattern);
const reservedNames = new Set<string>(CREDENTIAL_ENV.reserved_names);
const reserved = (name: string) => reservedNames.has(name) || CREDENTIAL_ENV.reserved_prefixes.some((prefix) => name.startsWith(prefix));
type Db = pg.Pool | pg.PoolClient;
type Stored = Omit<Environment, keyof EnvironmentConfig | 'image_approved' | 'created_at' | 'updated_at'> & {
  config: EnvironmentConfig; created_at: Date; updated_at: Date;
};
const select = `SELECT e.id,e.name,e.description,e.is_default,e.created_at,e.updated_at,
  v.id AS version_id,v.version,v.image_digest,v.config FROM environments e`;

function invalid(field: string, message = 'is invalid'): never {
  throw new HttpError(422, 'environment.invalid', `${field} ${message}`);
}
function denied(): never { throw new HttpError(403, 'auth.denied', 'runner resource unavailable'); }
function missing(): never { throw new HttpError(404, 'environment.not_found', 'environment unavailable'); }

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) invalid('configuration');
  return value as Record<string, unknown>;
}

const text = (value: unknown, max: number): value is string =>
  typeof value === 'string' && !value.includes('\0') && Buffer.byteLength(value) <= max;
const int = (value: unknown, min: number, max: number) =>
  typeof value === 'number' && Number.isSafeInteger(value) && value >= min && value <= max;
export const budgetCost = (value: unknown): value is number =>
  typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= 99_999_999.999999;
export const budgetThreshold = (value: unknown): value is number => int(value, 1, 100);

function variables(value: unknown, field: 'env' | 'credential_env') {
  const entries = Object.entries(object(value));
  if (entries.length > 64) invalid(field, 'supports at most 64 variables');
  for (const [name, item] of entries) {
    if (!variableName.test(name) || name.length > CREDENTIAL_ENV.max_name_length || reserved(name)) invalid(field, 'contains an invalid or reserved name');
    if (field === 'credential_env' ? !(typeof item === 'string' && /^[a-zA-Z0-9_.:-]{1,128}$/.test(item)) : !text(item, 4096))
      invalid(`${field}.${name}`);
    if (field === 'env' && findSecretShapedValues(item).length) invalid(`env.${name}`, 'looks secret; use a credential');
  }
  if (Buffer.byteLength(JSON.stringify(value)) > 32_768) invalid(field, 'is too large');
  return Object.fromEntries(entries) as Record<string, string>;
}

export function environmentConfig(input: unknown, prior = defaults): EnvironmentConfig & { name?: string; description?: string } {
  const change = object(input);
  const next = { ...prior, ...change };
  if (Object.keys(change).some((key) => key !== 'name' && key !== 'description' && !Object.hasOwn(defaults, key))) invalid('configuration');
  if ('name' in change && (!text(change.name, 128) || !change.name.trim() || [...change.name].some((ch) => ch.charCodeAt(0) < 32 || ch.charCodeAt(0) === 127))) invalid('name');
  if ('description' in change && !text(change.description, 2048)) invalid('description');
  if (next.image !== null && !text(next.image, 512)) invalid('image');
  if (next.setup !== null && (!text(next.setup, 65_536) || findSecretShapedValues(next.setup).length)) invalid('setup', 'must be a script without secrets');
  if (!int(next.setup_timeout_s, 1, 600)) invalid('setup_timeout_s', 'must be between 1 and 600');
  if (!int(next.timeout_s, 1, 86_400)) invalid('timeout_s', 'must be between 1 and 86400');
  if (!sizes.includes(next.size as typeof sizes[number])) invalid('size');
  const env = variables(next.env, 'env');
  const credential_env = variables(next.credential_env, 'credential_env');
  if (Object.keys(credential_env).some((name) => Object.hasOwn(env, name))) invalid('credential_env', 'has a name already defined in env');
  const budgets = object(next.budgets);
  for (const [key, value] of Object.entries(budgets)) {
    if (key === 'concurrency' ? value !== null && !int(value, 0, 2_147_483_647)
      : key === 'alert_threshold_percent' ? value !== null && !budgetThreshold(value)
        : !['cost_per_day_usd', 'cost_per_month_usd', 'max_step_cost_usd', 'max_run_cost_usd'].includes(key) ||
          (value !== null && !budgetCost(value))) invalid(`budgets.${key}`);
  }
  return { ...next, env, credential_env, egress: next.egress === null ? null :
    validateEgress(next.egress as EnvironmentConfig['egress'] & object), budgets } as ReturnType<typeof environmentConfig>;
}

export async function registryList(db: Db, org: string) {
  const { rows: [row] } = await db.query<{ registries: string[] }>(
    'SELECT registries FROM environment_registries WHERE org_id = $1', [org]);
  return row?.registries ?? [];
}

export function installationImage() {
  const image = process.env.RUNNER_SANDBOX_IMAGE;
  if (image && process.env.RUNNER_SANDBOX_BACKEND === 'kubernetes' && !digestImage(image))
    throw new Error('RUNNER_SANDBOX_IMAGE must be pinned by digest');
  return image && digestImage(image) ? image : null;
}

export function clusterApproved(image: string) {
  return process.env.RUNNER_SANDBOX_BACKEND !== 'kubernetes' ||
    (process.env.RUNNER_SANDBOX_APPROVED_IMAGES ?? '').split(',').includes(image);
}

export function approvedImage(image: string, allowed: string[]) {
  const host = imageHost(image);
  if (!host || !allowed.includes(host) || !digestImage(image)) return false;
  return clusterApproved(image);
}

export async function environmentPin(db: Db, org: string, id?: string | null, versionId?: string, legacy = false) {
  const { rows: [row] } = await db.query<Stored>(
    `${select} JOIN environment_versions v ON v.environment_id = e.id
     WHERE e.org_id = $1 AND ($2::text IS NULL OR e.id = $2)
       AND ($3::text IS NULL AND e.deleted_at IS NULL AND ($2 IS NOT NULL OR e.is_default) OR v.id = $3)
       AND (NOT $4::boolean OR v.version = 1)
     ORDER BY v.version DESC LIMIT 1`, [org, id ?? null, versionId ?? null, legacy],
  );
  return row;
}

async function audit(client: pg.PoolClient, actor: object, action: string, id: string, detail = {}) {
  await client.query(
    'INSERT INTO audit_log(id,actor,action,resource_type,resource_id,detail) VALUES ($1,$2,$3,$4,$5,$6)',
    [randomUUID(), JSON.stringify(actor), action, 'environment', id, JSON.stringify(detail)],
  );
}

export async function seedEnvironment(pool: pg.Pool, org: string, digest: string) {
  await withTransaction(pool, async (client) => {
    const { rowCount: changed } = await client.query(
      `INSERT INTO environment_installation_images(org_id,image_digest) VALUES ($1,$2)
       ON CONFLICT (org_id) DO UPDATE SET image_digest = EXCLUDED.image_digest
       WHERE environment_installation_images.image_digest <> EXCLUDED.image_digest`, [org, digest],
    );
    if (!changed) return;
    const { rows: [environment] } = await client.query<{ id: string }>(
      `INSERT INTO environments(id,org_id,name,description,is_default)
       VALUES ($1,$2,'Default','Installation sandbox image',true)
       ON CONFLICT (org_id) WHERE is_default DO NOTHING RETURNING id`,
      [randomUUID(), org],
    );
    if (!environment) {
      await audit(client, { kind: 'system' }, 'environment.installation_image_changed', org, { image_digest: digest });
      return;
    }
    await client.query(
      'INSERT INTO environment_versions(id,environment_id,version,config,image_digest) VALUES ($1,$2,1,$3,$4)',
      [randomUUID(), environment.id, JSON.stringify(defaults), digest],
    );
    await audit(client, { kind: 'system' }, 'environment.seeded', environment.id);
  });
}

async function validateCredentials(client: Db, org: string, config: EnvironmentConfig) {
  const ids = [...new Set(Object.values(config.credential_env))];
  if (!ids.length) return;
  const { rowCount } = await client.query(
    `SELECT id FROM credentials WHERE org_id = $1 AND id = ANY($2::text[]) AND status <> 'revoked'
      AND delivery ? $3 AND type = 'api-key'
      AND jsonb_typeof(config->'hosts') = 'array' AND config->'hosts' <> '[]'::jsonb`, [org, ids, CREDENTIAL_ENV.required_delivery],
  );
  if (rowCount !== ids.length) invalid('credential_env', 'requires a proxy credential with bound hosts');
}

function present(row: Stored, allowed: string[]) {
  const { config, ...data } = row;
  const image_approved = config.image !== null ? approvedImage(row.image_digest ?? '', allowed)
    : clusterApproved(row.image_digest ?? '');
  return { ...data, ...config, image_approved };
}

export function environmentResources(pool: pg.Pool) {
  return async ({ method, body, claims }: PublicRequest, resource: string, id?: string, operation?: string) => {
    const org = claims.org_id;
    if (method === 'GET') {
      const allowed = await registryList(pool, org);
      if (resource === 'environment-registries') return { body: { items: allowed } };
      if (id) return { body: present(await environmentPin(pool, org, id) ?? missing(), allowed) };
      const { rows } = await pool.query<Stored>(
        `${select} JOIN LATERAL (
           SELECT * FROM environment_versions WHERE environment_id = e.id ORDER BY version DESC LIMIT 1
         ) v ON true WHERE e.org_id = $1 AND e.deleted_at IS NULL ORDER BY e.is_default DESC,e.name`, [org]);
      return { body: { items: rows.map((row) => present(row, allowed)) } };
    }
    if (!claims.session_hash || claims.principal !== 'human' || authorize(claims, 'widen', { kind: 'org' }) !== 'allow') denied();
    const actor = { kind: claims.principal, id: claims.user_id };
    if (resource === 'environment-registries') {
      if (claims.org_role !== 'owner') denied();
      const { items } = object(body);
      if (!Array.isArray(items) || items.length > 64 || items.some((item) => typeof item !== 'string' || !registryHost(item)))
        invalid('items', 'must contain allowed registry hostnames');
      const registries = [...new Set(items as string[])].sort();
      await withTransaction(pool, async (client) => {
        await client.query(
          `INSERT INTO environment_registries (org_id,registries) VALUES ($1,$2)
           ON CONFLICT (org_id) DO UPDATE SET registries = EXCLUDED.registries`, [org, JSON.stringify(registries)],
        );
        await audit(client, actor, 'environment.registries_updated', org, { registries });
      });
      return { body: { items: registries } };
    }

    const prior = id ? await environmentPin(pool, org, id) ?? missing() : undefined;
    if (method === 'DELETE') {
      if (prior?.is_default) throw new HttpError(409, 'environment.default_required', 'the default environment cannot be deleted');
      await withTransaction(pool, async (client) => {
        const { rowCount } = await client.query(
          'UPDATE environments SET deleted_at = now() WHERE id = $1 AND org_id = $2 AND deleted_at IS NULL', [id, org]);
        if (!rowCount) missing();
        await audit(client, actor, 'environment.deleted', id!);
      });
      return { status: 204 };
    }
    const allowed = await registryList(pool, org);
    const { name = prior?.name, description = prior?.description ?? '', ...config } =
      environmentConfig(operation ? {} : body, prior?.config);
    if (!name) invalid('name', 'is required');
    const refresh = operation === 'resolve-digest' || !prior || config.image !== prior.config.image;
    const digest = refresh
      ? config.image ? await resolveImage(config.image, allowed)
        : (await pool.query<{ image_digest: string }>(
          'SELECT image_digest FROM environment_installation_images WHERE org_id = $1', [org])).rows[0]?.image_digest ?? null
      : prior.image_digest;
    if (!digest) throw new HttpError(503, 'environment.default_unavailable', 'installation sandbox image unavailable');
    const environmentId = id ?? randomUUID();
    const versionId = randomUUID();
    try {
      return await withTransaction(pool, async (client) => {
        if (prior) {
          const locked = await client.query(
            'SELECT 1 FROM environments WHERE id = $1 AND org_id = $2 AND deleted_at IS NULL FOR UPDATE', [id, org]);
          if (!locked.rowCount) missing();
          const latest = await environmentPin(client, org, id);
          if (latest?.version_id !== prior.version_id)
            throw new HttpError(409, 'environment.changed', 'environment changed, refresh and try again');
          await client.query('UPDATE environments SET name = $2,description = $3,updated_at = now() WHERE id = $1',
            [id, name, description]);
        } else {
          await client.query('INSERT INTO environments(id,org_id,name,description) VALUES ($1,$2,$3,$4)',
            [environmentId, org, name, description]);
        }
        await validateCredentials(client, org, config);
        const registries = await registryList(client, org);
        if (config.image && !registries.includes(imageHost(config.image) ?? ''))
          throw new HttpError(422, 'environment.registry_not_allowed', 'use an image from an allowed registry');
        await client.query(
          'INSERT INTO environment_versions(id,environment_id,version,config,image_digest) VALUES ($1,$2,$3,$4,$5)',
          [versionId, environmentId, (prior?.version ?? 0) + 1, JSON.stringify(config), digest],
        );
        await audit(client, actor, operation ? 'environment.digest_resolved' : prior ? 'environment.updated' : 'environment.created',
          environmentId, { version_id: versionId, image_digest: digest });
        const row = await environmentPin(client, org, environmentId, versionId);
        return { status: prior ? 200 : 201, body: present(row!, registries) };
      });
    } catch (error) {
      if ((error as { code?: string }).code === '23505')
        throw new HttpError(409, 'environment.name_taken', 'an environment already has this name');
      throw error;
    }
  };
}
