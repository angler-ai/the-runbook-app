import { oauthUrl } from '@runbook/runner-shared/oauth-binding';
import { groupActivity } from './activity.ts';
import { createHash, randomUUID } from 'node:crypto';
import type pg from 'pg';
import { canonicalJson } from '@runbook/runner-shared/binding';
import type { Config } from '@runbook/runner-shared/config';
import { LABELS, type UserAssertionClaims } from '@runbook/runner-shared/contracts/core';
import { CREDENTIAL_TYPES } from '@runbook/runner-shared/credential-types';
import { withTransaction } from '@runbook/runner-shared/db';
import { HttpError } from '@runbook/runner-shared/http';
import { createLogger } from '@runbook/runner-shared/log';
import { findSecretShapedValues } from '@runbook/runner-shared/secret-shape';
import { authorize } from '../authorize.ts';
import type { PublicRequest } from '../server.ts';
import { createProposal } from './proposals.ts';
import { budgetThreshold } from './environments.ts';
import { sharedWith, validateEgress } from './agents.ts';
import { pinnedTools, toolDrift, toolSummaries } from './catalog-tools.ts';

const catalogs = {
  groups: { table: 'groups', id: 'group_id', fields: ['name', 'projects', 'concurrency', 'cost_per_day_usd', 'cost_per_month_usd', 'alert_threshold_percent', 'sandbox', 'compute_rate'] },
  providers: { table: 'providers', id: 'id', fields: ['name', 'type', 'auth', 'credential_id', 'endpoint', 'region', 'models', 'sharing'] },
  profiles: { table: 'profiles', id: 'id', fields: ['name', 'provider_id', 'model', 'options', 'sharing'] },
  'mcp-servers': { table: 'mcp_servers', id: 'id', fields: ['name', 'url', 'transport', 'credential_id', 'tool_labels', 'never_allowed', 'sharing'] },
  policy: { table: 'policy', id: 'id', fields: ['org_concurrency', 'org_cost_per_day_usd', 'org_cost_per_month_usd', 'agent_concurrency',
    'agent_max_cost_usd', 'default_egress', 'credential_max_age_days', 'time_zone', 'project_limits', 'run_max_cost_usd', 'alert_threshold_percent'] },
} as const;

const jsonColumns = new Set(['projects', 'sandbox', 'compute_rate', 'models', 'sharing', 'options', 'tool_labels', 'never_allowed',
  'default_egress', 'project_limits']);
const mcpPresets = [
  { name: 'linear-readonly', url: 'https://mcp.linear.app/mcp/readonly', transport: 'streamable-http',
    credential: { type: 'mcp-bearer', config: { url: 'https://mcp.linear.app/mcp/readonly' } } },
  { name: 'github-readonly', url: 'https://api.githubcopilot.com/mcp/readonly', transport: 'streamable-http',
    credential: { type: 'mcp-bearer', config: { url: 'https://api.githubcopilot.com/mcp/readonly' } } },
];

type Catalog = keyof typeof catalogs;
type Row = Record<string, unknown>;
type McpSource = { url: string; transport: 'streamable-http' | 'sse'; credential_id?: string };

export function isCatalog(value: string | undefined): value is Catalog {
  return !!value && Object.hasOwn(catalogs, value);
}

export function catalogPresets(resource: 'credentials' | 'mcp-servers', claims: UserAssertionClaims) {
  if (authorize(claims, 'read', { kind: 'org' }) !== 'allow') denied();
  return { body: { items: resource === 'credentials' ? Object.values(CREDENTIAL_TYPES) : mcpPresets } };
}

function invalid(): never { throw new HttpError(422, 'catalog.invalid', 'invalid runner catalog configuration'); }
function denied(): never { throw new HttpError(403, 'auth.denied', 'runner resource unavailable'); }
function absent(): never { throw new HttpError(404, 'catalog.not_found', 'runner catalog entry unavailable'); }

function record(value: unknown): Row {
  if (!value || typeof value !== 'object' || Array.isArray(value)) invalid();
  return value as Row;
}
function identifier(value: unknown): value is string {
  return typeof value === 'string' && /^[A-Za-z0-9_.:-]{1,128}$/.test(value);
}
function name(value: unknown): boolean {
  return typeof value === 'string' && value.length <= 128 && value.trim().length > 0;
}
function identifiers(value: unknown): boolean {
  return Array.isArray(value) && value.length <= 1000 && value.every(identifier);
}
function cost(value: unknown): boolean {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 && value < 100_000_000;
}
function count(value: unknown): boolean {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 && value <= 2_147_483_647;
}
function url(value: unknown, http = false): string {
  if (typeof value !== 'string' || value.length > 2048) invalid();
  try {
    const parsed = new URL(value);
    if (parsed.username || parsed.password || parsed.search || parsed.hash ||
        (parsed.protocol !== 'https:' && !(http && parsed.protocol === 'http:'))) invalid();
    return parsed.origin + parsed.pathname.replace(/\/+$/, '');
  } catch {
    return invalid();
  }
}
function sharing(value: unknown): void {
  const share = record(value);
  if (Object.keys(share).some((key) => key !== 'projects') ||
      (share.projects !== 'none' && share.projects !== 'all' && !identifiers(share.projects))) invalid();
}

function mcpSource(row: Row): McpSource {
  if (typeof row.url !== 'string' || (row.transport !== 'streamable-http' && row.transport !== 'sse')) invalid();
  return { url: row.url, transport: row.transport, ...(identifier(row.credential_id) && { credential_id: row.credential_id }) };
}
function hash(value: unknown): Buffer {
  return createHash('sha256').update(canonicalJson(value)).digest();
}

function validateToolAccess(change: Row) {
  if ('tool_labels' in change && Object.entries(record(change.tool_labels))
    .some(([tool, label]) => !name(tool) || !LABELS.some((candidate) => candidate === label))) invalid();
  if ('never_allowed' in change && (!Array.isArray(change.never_allowed) || !change.never_allowed.every(name))) invalid();
}

async function validate(client: pg.PoolClient, catalog: Catalog, change: Row, current: Row, orgId: string) {
  const row = { ...current, ...change };
  if (catalog !== 'policy' && !name(row.name)) invalid();
  if ('sharing' in change) sharing(change.sharing);
  for (const [key, value] of Object.entries(change)) {
    if (['concurrency', 'org_concurrency', 'agent_concurrency', 'credential_max_age_days'].includes(key) &&
        !count(value) && !(value === null && (['concurrency', 'org_concurrency', 'credential_max_age_days'].includes(key)))) invalid();
    if (key === 'alert_threshold_percent' && value !== null && !budgetThreshold(value)) invalid();
    if (key.endsWith('_usd') && !cost(value) &&
        !(value === null && key !== 'agent_max_cost_usd')) invalid();
  }
  if (catalog === 'groups') {
    if ('projects' in change && !identifiers(change.projects)) invalid();
    for (const key of ['sandbox', 'compute_rate']) if (key in change) record(change[key]);
  } else if (catalog === 'policy') {
    if ('default_egress' in change) change.default_egress = validateEgress(change.default_egress as Parameters<typeof validateEgress>[0]);
    if ('time_zone' in change) {
      if (typeof change.time_zone !== 'string') invalid();
      try { new Intl.DateTimeFormat('en', { timeZone: change.time_zone }); } catch { invalid(); }
    }
    if ('project_limits' in change) {
      for (const [project, limits] of Object.entries(record(change.project_limits))) {
        if (!identifier(project)) invalid();
        for (const [key, value] of Object.entries(record(limits))) {
          if (key === 'concurrency' ? value !== null && !count(value)
            : key === 'alert_threshold_percent' ? value !== null && !budgetThreshold(value)
              : !['cost_per_day_usd', 'cost_per_month_usd'].includes(key) || (value !== null && !cost(value))) invalid();
        }
      }
    }
  } else if (catalog === 'providers') {
    if (row.auth === 'attached' || row.type === 'vertex' || row.type === 'bedrock') {
      throw new HttpError(422, 'catalog.attached_identity_disabled', 'attached model identity is unavailable until the vault supports it');
    }
    if (!['anthropic', 'openai-compatible', 'vertex-gemini'].includes(String(row.type)) || row.auth !== 'stored' ||
        !identifier(row.credential_id) || !Array.isArray(row.models) || row.models.length === 0) invalid();
    const endpoint = url(row.endpoint, row.type === 'openai-compatible' && process.env.RUNNER_ALLOW_HTTP_MODEL_ENDPOINTS === 'true');
    if ('endpoint' in change) change.endpoint = endpoint;
    if (row.region != null && !identifier(row.region)) invalid();
    const models = row.models.map(record);
    if (new Set(models.map((model) => model.id)).size !== models.length ||
        models.some((model) => (model.cache_write_usd_per_million !== undefined && !cost(model.cache_write_usd_per_million)) || !name(model.id) || !cost(model.input_usd_per_million) || !cost(model.output_usd_per_million) ||
          ('cached_usd_per_million' in model && !cost(model.cached_usd_per_million)) ||
          Object.keys(model).some((key) => !['id', 'input_usd_per_million', 'output_usd_per_million', 'cached_usd_per_million', 'cache_write_usd_per_million'].includes(key)))) invalid();
    const found = await client.query(
      `SELECT id FROM credentials WHERE id = $1 AND org_id = $2 AND type = 'model-provider' AND status <> 'revoked'
       AND binding->>'provider' = $3 AND binding->>'endpoint' = $4`, [row.credential_id, orgId, row.type, endpoint],
    );
    if (!found.rowCount) invalid();
  } else if (catalog === 'profiles') {
    if (!identifier(row.provider_id) || !name(row.model)) invalid();
    if ('options' in change) {
      const options = record(change.options);
      if ('effort' in options && options.effort !== 'low' && options.effort !== 'medium' && options.effort !== 'high') invalid();
      if ('max_response_bytes' in options && (!count(options.max_response_bytes) || options.max_response_bytes === 0)) invalid();
      if ('max_output_tokens' in options && (!count(options.max_output_tokens) || options.max_output_tokens === 0)) invalid();
    }
    const found = await client.query(
      `SELECT 1 FROM providers WHERE id = $1 AND models @> jsonb_build_array(jsonb_build_object('id', $2::text))`,
      [row.provider_id, row.model],
    );
    if (!found.rowCount) invalid();
  } else {
    let endpoint: string;
    if (row.credential_id == null) {
      endpoint = url(row.url);
    } else {
      if (!identifier(row.credential_id)) invalid();
      const { rows: [credential] } = await client.query<{ type: string; bound_url: string }>(
        `SELECT type, binding->>'url' AS bound_url FROM credentials WHERE id = $1 AND org_id = $2
          AND type IN ('mcp-bearer', 'mcp-oauth') AND status <> 'revoked'`, [row.credential_id, orgId],
      );
      if (!credential) invalid();
      try {
        endpoint = credential.type === 'mcp-oauth' ? oauthUrl(row.url, 'MCP URL') : url(row.url);
      } catch {
        invalid();
      }
      if (credential.bound_url !== endpoint) invalid();
    }
    if ('url' in change) change.url = endpoint;
    if (row.transport !== 'streamable-http' && row.transport !== 'sse') invalid();
    validateToolAccess(change);
  }
}

const MAX_MCP_TOOLS = 500;
// Accommodate large provider schemas while leaving room under core's 4 MiB response limit.
const MAX_MCP_SNAPSHOT_BYTES = 1024 * 1024;
const log = createLogger('api');

function isObject(value: unknown): value is Row {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function validateToolList(payload: unknown, serverId: string) {
  function reject(reason: string): never {
    // Indexes identify offending tools without echoing untrusted names or schemas.
    log.warn('MCP discovery validation failed', { server_id: serverId, reason });
    throw new HttpError(422, 'catalog.invalid_tool_list', reason);
  }

  if (!isObject(payload) || !Array.isArray(payload.tools)) reject('MCP discovery response must contain a tools array');
  const tools: unknown[] = payload.tools;
  if (tools.length > MAX_MCP_TOOLS) reject(`MCP tool list exceeds the ${MAX_MCP_TOOLS}-tool limit (${tools.length})`);
  const json = JSON.stringify(tools);
  const bytes = Buffer.byteLength(json);
  if (bytes > MAX_MCP_SNAPSHOT_BYTES) reject(`MCP tool list exceeds the ${MAX_MCP_SNAPSHOT_BYTES}-byte limit (${bytes} bytes)`);

  const names = new Map<string, number>();
  for (const [index, tool] of tools.entries()) {
    const position = index + 1;
    if (!isObject(tool)) reject(`MCP tool #${position} must be an object`);
    if (typeof tool.name !== 'string' || !name(tool.name)) reject(`MCP tool #${position} name must be nonblank and at most 128 characters`);
    const previous = names.get(tool.name);
    if (previous !== undefined) reject(`MCP tool #${position} duplicates the name of tool #${previous}`);
    names.set(tool.name, position);
    if (!isObject(tool.inputSchema) || tool.inputSchema.type !== 'object') {
      reject(`MCP tool #${position} inputSchema must be a JSON Schema object with type "object"`);
    }
  }
  return { tools, json };
}

export function catalogResources(pool: pg.Pool, config: Config, sync: () => Promise<void>) {
  async function discover(source: McpSource, serverId: string, assertion: PublicRequest['assertion']) {
    const oauth = source.credential_id && (await pool.query(
      "SELECT id FROM credentials WHERE type = 'mcp-oauth' AND id = $1", [source.credential_id],
    )).rowCount;
    if (oauth && !assertion) denied();
    const path = oauth ? `/internal/mcp/${serverId}/oauth-discover` : '/internal/mcp/discover';
    let payload: unknown;
    try {
      const response = await fetch(config.urls.vault + path, {
        method: 'POST', headers: { authorization: `Bearer ${config.outboundTokens.vault}`, 'content-type': 'application/json',
          ...(oauth && { 'runbook-assertion': assertion!.token }) },
        body: oauth ? new Uint8Array(assertion!.rawBody) : JSON.stringify(source),
        redirect: 'error', signal: AbortSignal.timeout(25_000),
      });
      if (!response.ok) throw new Error();
      payload = await response.json();
    } catch {
      throw new HttpError(502, 'catalog.discovery_unavailable', 'MCP tool discovery unavailable');
    }
    return validateToolList(payload, serverId);
  }
  return async (request: PublicRequest, catalog: Catalog, id?: string, operation?: string, transaction?: pg.PoolClient) => {
    const { method, claims, body } = request;
    const authorization = authorize(claims, method === 'GET' ? 'read' : 'widen', { kind: 'org' });
    if (authorization === 'deny') denied();
    const schema = catalogs[catalog];
    const active = catalog === 'mcp-servers' ? ' AND deleted_at IS NULL' : '';
    const fields: readonly string[] = schema.fields;
    function readChange() {
      const input = record(body);
      if (Object.keys(input).some((key) => !fields.includes(key) && !(catalog === 'groups' && !id && key === 'group_id')) ||
          findSecretShapedValues(input).length) invalid();
      return input;
    }
    if (authorization === 'proposal') {
      if (operation || (method !== 'POST' && method !== 'PATCH' && !(catalog === 'policy' && method === 'PUT'))) denied();
      return { status: 202, body: await createProposal(pool, claims, {
        kind: 'catalog.change', targetType: catalog, targetId: id ?? null, diff: { resource: catalog, change: readChange() },
      }) };
    }
    const byId = `${schema.id} = $1${active}`;
    const orgAdmin = claims.org_role === 'owner' || claims.org_role === 'admin';
    const projects = Object.keys(claims.project_roles);

    const select = `SELECT * FROM ${schema.table}`;
    if (method === 'GET') {
      if (catalog === 'policy') {
        if (!orgAdmin) denied();
        return { body: (await pool.query(select)).rows[0] };
      }
      const read = catalog === 'profiles'
        ? `SELECT profiles.*, CASE WHEN profiles.verified_at >= profiles.updated_at
             AND profiles.verified_at >= providers.updated_at AND profile_credential.status = 'active'
             THEN profiles.verified_at END AS verified_at
           FROM profiles JOIN providers ON providers.id = profiles.provider_id
           LEFT JOIN credentials profile_credential ON profile_credential.id = providers.credential_id`
        : select;
      const alias = catalog === 'profiles' ? 'profiles.' : '';
      const visibility = orgAdmin ? '' : catalog === 'groups'
        ? ' AND projects ?| $2::text[]'
        : ` AND (${alias}sharing->>'projects' = 'all' OR (${alias}sharing->'projects') ?| $2::text[])`;
      const { rows } = await pool.query<Row>(
        `${read} WHERE ($1::text IS NULL OR ${alias}${schema.id} = $1)${active}${visibility} ORDER BY ${alias}name LIMIT 1000`,
        orgAdmin ? [id ?? null] : [id ?? null, projects],
      );
      if (catalog === 'groups' && orgAdmin && rows.length > 0) {
        const metrics = await groupActivity(pool, rows.map((row) => String(row.group_id)));
        for (const row of rows) Object.assign(row, metrics.get(String(row.group_id)) ??
          { busy_ms: 0, idle_ms: 0, idle_stops: 0 });
      }
      const visible = orgAdmin ? rows : rows.flatMap<Row>((row) => {
        if (catalog === 'groups') return [{ ...row, projects: (row.projects as string[]).filter((project) => projects.includes(project)) }];
        const shared = projects.filter((project) => sharedWith(row.sharing, project));
        return shared.length ? [{ ...row, sharing: { projects: shared } }] : [];
      });
      if (catalog === 'mcp-servers') {
        if (id && operation) {
          const row = visible[0];
          if (!row) absent();
          if (operation === 'tools') return { body: await pinnedTools(pool, id, row.pinned_snapshot_id) };
          if (operation === 'drift') return { body: await toolDrift(pool, id, row.pinned_snapshot_id, hash(mcpSource(row))) };
        }
        if (visible.length > 0) {
          const summaries = await toolSummaries(pool, visible.map((row) => ({
            id: String(row.id), sourceHash: hash(mcpSource(row)),
          })));
          for (const row of visible) Object.assign(row, summaries.get(String(row.id)));
        }
      }
      return id ? { status: visible.length ? 200 : 404, body: visible[0] } : { body: { items: visible } };
    }

    const audit = async (db: pg.PoolClient, action: string, resourceId: string) => {
      await db.query(
        'INSERT INTO audit_log(id, actor, action, resource_type, resource_id) VALUES ($1,$2,$3,$4,$5)',
        [randomUUID(), JSON.stringify({ kind: 'user', id: claims.user_id }), `catalog.${action}`, catalog, resourceId],
      );
    };
    if (id && operation === 'discover') {
      const { rows: [row] } = await pool.query<Row>(`${select} WHERE ${byId}`, [id]);
      if (!row) absent();
      const source = mcpSource(row);
      const { tools, json } = await discover(source, id, request.assertion);
      const toolHash = hash(tools);
      const snapshotId = randomUUID();
      await withTransaction(pool, async (client) => {
        const { rows: [current] } = await client.query<Row>(`${select} WHERE ${byId} FOR UPDATE`, [id]);
        if (!current || !hash(mcpSource(current)).equals(hash(source))) {
          throw new HttpError(409, 'catalog.changed', 'MCP server changed during discovery');
        }
        await client.query(
          `INSERT INTO mcp_tool_snapshots(id, server_id, hash, tools, pinned_by, source_hash) VALUES ($1,$2,$3,$4,$5,$6)`,
          [snapshotId, id, toolHash, json, claims.user_id, hash(source)],
        );
        await audit(client, 'discovered', id);
      });
      return { body: { snapshot_id: snapshotId, hash: toolHash.toString('hex'), tools } };
    }

    const write = async (client: pg.PoolClient) => {
      if (id && operation === 'pin') {
        const input = record(body);
        const snapshotId = input.snapshot_id;
        validateToolAccess(input);
        if (!identifier(snapshotId) || findSecretShapedValues({
          tool_labels: input.tool_labels, never_allowed: input.never_allowed,
        }).length) invalid();
        const { rows: [current] } = await client.query<Row>(`${select} WHERE ${byId} FOR UPDATE`, [id]);
        if (!current) absent();
        const pinned = await client.query(
          `UPDATE mcp_servers SET pinned_snapshot_id = $2, updated_at = now(),
             tool_labels = coalesce($4::jsonb, tool_labels), never_allowed = coalesce($5::jsonb, never_allowed)
           WHERE id = $1 AND EXISTS (
             SELECT 1 FROM mcp_tool_snapshots WHERE id = $2 AND server_id = $1 AND source_hash = $3)
           RETURNING *`, [id, snapshotId, hash(mcpSource(current)),
             JSON.stringify(input.tool_labels) ?? null, JSON.stringify(input.never_allowed) ?? null],
        );
        if (!pinned.rowCount) throw new HttpError(409, 'catalog.invalid_snapshot', 'snapshot unavailable for this server');
        await audit(client, 'pinned', id);
        return { body: pinned.rows[0] };
      }
      if (id && method === 'DELETE') {
        const deletion = await client.query(catalog === 'mcp-servers'
          ? `UPDATE mcp_servers SET deleted_at = now(), pinned_snapshot_id = NULL WHERE ${byId}`
          : `DELETE FROM ${schema.table} WHERE ${byId}`, [id]);
        if (!deletion.rowCount) absent();
        await audit(client, 'deleted', id);
        return { status: 204 };
      }
      const input = readChange();
      const policy = catalog === 'policy';
      const resourceId = policy ? 1 : id ?? (catalog === 'groups' ? input.group_id : randomUUID());
      if ((!policy && !identifier(resourceId)) || operation) invalid();
      const change = Object.fromEntries(Object.entries(input).filter(([key]) => key !== 'group_id'));
      const { rows: [current] } = id || policy ? await client.query<Row>(`${select} WHERE ${byId} FOR UPDATE`, [resourceId]) : { rows: [] };
      if ((id || policy) && !current) absent();
      await validate(client, catalog, change, current ?? {}, claims.org_id);
      // Older clients also send this field to the runner. Only the core's
      // registry snapshot can change the mirrored assignments.
      if (catalog === 'groups') delete change.projects;
      if (current && catalog === 'mcp-servers' &&
          !hash(mcpSource(current)).equals(hash(mcpSource({ ...current, ...change })))) change.pinned_snapshot_id = null;

      const keys = Object.keys(change);
      if (current && !keys.length) return { body: current };
      const values = [resourceId, ...keys.map((key) => jsonColumns.has(key) ? JSON.stringify(change[key]) : change[key])];
      const saved = await client.query(current
        ? `UPDATE ${schema.table} SET ${keys.map((key, index) => `${key} = $${index + 2}`).join(', ')},
           updated_at = now() WHERE ${byId} RETURNING *`
        : `INSERT INTO ${schema.table}(${schema.id}, ${keys.join(', ')}) VALUES (${values.map((_, i) => `$${i + 1}`).join(', ')}) RETURNING *`,
      values);
      await audit(client, current ? 'updated' : 'created', String(resourceId));
      return { status: current ? 200 : 201, body: saved.rows[0] };
    };
    const result = await (transaction ? write(transaction) : withTransaction(pool, write)).catch((error: unknown) => {
      if (['23503', '23505'].includes((error as { code?: string }).code ?? '')) {
        throw new HttpError(409, 'catalog.conflict', 'runner catalog entry conflicts with existing configuration');
      }
      throw error;
    });
    if (!transaction) await sync();
    return result;
  };
}
