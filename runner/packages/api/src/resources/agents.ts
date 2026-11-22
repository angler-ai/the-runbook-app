import { randomUUID } from 'node:crypto';
import type pg from 'pg';
import type { components } from '@runbook/runner-shared/core-client';
import { type AgentCeiling, type Delivery, type Label } from '@runbook/runner-shared/contracts/core';
import { withTransaction } from '@runbook/runner-shared/db';
import type { AgentLimits } from '../ledger/limits.ts';
import { HttpError } from '@runbook/runner-shared/http';

type Credential = { id: string; name: string; sharing: unknown; label?: Label; delivery: Delivery[]; status?: string };

export interface AgentConfig {
  profiles: string[];
  instructions?: string;
  skills?: string[];
  skill_pins?: components['schemas']['RunnerSkillPin'][];
  sandbox?: { size: 'small' | 'medium' | 'large' };
  credentials: { credential_id: string; delivery: Delivery }[];
  limits: Pick<AgentCeiling, 'max_cost_usd' | 'max_tokens' | 'timeout_s'> & AgentLimits & {
    max_turns?: number; idle_timeout_s?: number; tool_timeout_s?: number; stream_stall_s?: number;
  };
  egress?: AgentCeiling['hosts'];
  mcp?: { server_id: string; tool: string }[];
}

interface Catalog {
  profiles: { id: string; name: string; model?: string; sharing?: unknown }[];
  credentials: Credential[];
  tools?: AgentCeiling['tools'];
}

export function sharedWith(sharing: unknown, projectId: string): boolean {
  const projects = (sharing as { projects?: unknown } | null)?.projects;
  return projects === 'all' || (Array.isArray(projects) && projects.includes(projectId));
}

export function credentialAllowed(credential: Credential, projectId: string, delivery: Delivery): boolean {
  return credential.status !== 'revoked' && sharedWith(credential.sharing, projectId) && credential.delivery.includes(delivery);
}

export function validateEgress(entries: AgentCeiling['hosts'] = []): AgentCeiling['hosts'] {
  if (!Array.isArray(entries) || entries.some((entry) => !entry || typeof entry.host !== 'string' ||
      entry.host.length > 253 || !entry.host.split('.').every((part) => /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/i.test(part)) ||
      (entry.label !== 'read' && entry.label !== 'write'))) {
    throw new HttpError(422, 'agent.invalid_egress', 'invalid agent egress');
  }
  return entries.map(({ host, label }) => ({ host: host.toLowerCase(), label }));
}

export function computeCeiling(config: AgentConfig, catalog: Catalog, projectId: string): AgentCeiling {
  const configuredProfiles = [...new Set(config.profiles)].map((id) => {
    const profile = catalog.profiles.find((item) => item.id === id);
    if (!profile || (profile.sharing !== undefined && !sharedWith(profile.sharing, projectId))) {
      throw new HttpError(422, 'agent.profile_unavailable', 'profile unavailable');
    }
    return profile;
  });
  const credentials = new Map<string, AgentCeiling['credentials'][number]>();
  for (const item of config.credentials) {
    const credential = catalog.credentials.find(({ id }) => id === item.credential_id);
    if (!credential || !credentialAllowed(credential, projectId, item.delivery)) {
      throw new HttpError(422, 'agent.credential_unavailable', 'credential unavailable');
    }
    const existing = credentials.get(credential.id);
    if (existing) {
      if (!existing.delivery.includes(item.delivery)) existing.delivery.push(item.delivery);
    } else {
      credentials.set(credential.id, { id: credential.id, name: credential.name,
        label: credential.label ?? 'destructive', delivery: [item.delivery] });
    }
  }
  const tools = (config.mcp ?? []).map(({ server_id: server, tool }) => {
    const entry = catalog.tools?.find(({ server_id, name }) => server_id === server && name === tool);
    if (!entry) throw new HttpError(422, 'agent.tool_unavailable', 'tool unavailable');
    return entry;
  });
  const { max_cost_usd, max_tokens, timeout_s, concurrency, cost_per_day_usd } = config.limits;
  const { max_turns, idle_timeout_s, tool_timeout_s, stream_stall_s } = config.limits;
  const bound = (value: number | undefined, max: number) => value !== undefined &&
    (!Number.isSafeInteger(value) || value < 1 || value > max);
  if (!Number.isFinite(max_cost_usd) || max_cost_usd < 0 || !Number.isSafeInteger(max_tokens) || max_tokens < 1 ||
      !Number.isSafeInteger(timeout_s) || timeout_s < 1 || timeout_s > 86400 ||
      bound(max_turns, 1000) || bound(idle_timeout_s, 86400) || bound(tool_timeout_s, 300) || bound(stream_stall_s, 3600) ||
      (concurrency !== undefined && (!Number.isSafeInteger(concurrency) || concurrency < 0)) ||
      (cost_per_day_usd !== undefined && (!Number.isFinite(cost_per_day_usd) || cost_per_day_usd < 0 || cost_per_day_usd >= 100_000_000))) {
    throw new HttpError(422, 'agent.invalid_limits', 'invalid agent limits');
  }
  const profiles = configuredProfiles.map(({ name }) => name);
  const profile_models = Object.fromEntries(configuredProfiles.flatMap(({ name, model }) => model ? [[name, model]] : []));
  return { profiles, ...(Object.keys(profile_models).length > 0 && { profile_models }), credentials: [...credentials.values()], tools, hosts: validateEgress(config.egress),
    max_cost_usd, max_tokens, timeout_s };
}

export async function loadCeiling(db: pg.Pool | pg.PoolClient, config: AgentConfig, projectId: string, orgId: string,
  omitUnavailable = false): Promise<AgentCeiling> {
  const [profiles, credentials, servers] = await Promise.all([
    db.query<{ id: string; name: string; model: string; sharing: unknown; provider_sharing: unknown; credential_id: string | null }>(
      `SELECT p.id, p.name, p.model, p.sharing, v.sharing AS provider_sharing, v.credential_id
       FROM profiles p JOIN providers v ON v.id = p.provider_id WHERE p.id = ANY($1)`, [config.profiles]),

    db.query<Credential>(
      `SELECT id, name, sharing, label, delivery, status FROM credentials
       WHERE org_id = $2 AND (id = ANY($1) OR id IN (
         SELECT v.credential_id FROM profiles p JOIN providers v ON v.id = p.provider_id WHERE p.id = ANY($3)
       ))`,
      [config.credentials.map(({ credential_id }) => credential_id), orgId, config.profiles],
    ),
    db.query<{ id: string; sharing: unknown; tools: { name: string }[]; tool_labels: Record<string, AgentCeiling['tools'][number]['label']>;
      never_allowed: string[] }>(
      `SELECT m.id, m.sharing, m.tool_labels, m.never_allowed, s.tools FROM mcp_servers m
       JOIN mcp_tool_snapshots s ON s.id = m.pinned_snapshot_id AND s.server_id = m.id
       WHERE m.id = ANY($1)`, [(config.mcp ?? []).map(({ server_id }) => server_id)],
    ),
  ]);
  const tools = servers.rows.filter((server) => sharedWith(server.sharing, projectId)).flatMap((server) =>
    server.tools.filter((tool) => !server.never_allowed.includes(tool.name)).map(({ name }) => ({
      server_id: server.id, name, label: server.tool_labels[name] ?? 'destructive' as const,
    })));
  const availableProfiles = profiles.rows.filter((profile) => sharedWith(profile.sharing, projectId) &&
    sharedWith(profile.provider_sharing, projectId) &&
    (!profile.credential_id || credentials.rows.some((credential) =>
      credential.id === profile.credential_id && credentialAllowed(credential, projectId, 'model'))));
  const catalog = { profiles: availableProfiles, credentials: credentials.rows, tools };
  config = { ...config, credentials: [...config.credentials, ...availableProfiles.flatMap(({ credential_id }) =>
    credential_id ? [{ credential_id, delivery: 'model' as const }] : [])] };
  if (omitUnavailable) {
    config = {
      ...config,
      profiles: config.profiles.filter((id) => catalog.profiles.some((profile) => profile.id === id && sharedWith(profile.sharing, projectId))),
      credentials: config.credentials.filter(({ credential_id, delivery }) =>
        catalog.credentials.some((credential) => credential.id === credential_id && credentialAllowed(credential, projectId, delivery))),
      mcp: config.mcp?.filter(({ server_id, tool }) =>
        tools.some(({ server_id: server, name }) => server === server_id && name === tool)),
    };
  }
  return computeCeiling(config, catalog, projectId);
}

interface AgentVersionInput {
  agentId: string; projectId: string; actorId: string; config: AgentConfig; ceiling: AgentCeiling;
}

export async function saveAgentVersion(pool: pg.Pool, input: AgentVersionInput) {
  return withTransaction(pool, (client) => saveAgentVersionInTransaction(client, input));
}

export async function saveAgentVersionInTransaction(client: pg.PoolClient, input: AgentVersionInput) {
  const locked = await client.query('SELECT id FROM agents WHERE id = $1 AND project_id = $2 FOR UPDATE', [
    input.agentId, input.projectId,
  ]);
  if (!locked.rowCount) throw new HttpError(404, 'agent.not_found', 'agent unavailable');
  const id = randomUUID();
  const result = await client.query<{ version: number }>(
    `INSERT INTO agent_versions(id, agent_id, version, config, ceiling, created_by)
     SELECT $1, $2, COALESCE(MAX(version), 0) + 1, $3, $4, $5 FROM agent_versions WHERE agent_id = $2
     RETURNING version`,
    [id, input.agentId, JSON.stringify(input.config), JSON.stringify(input.ceiling), input.actorId],
  );
  await client.query('UPDATE agents SET current_version_id = $1, updated_at = now() WHERE id = $2', [id, input.agentId]);
  await client.query(
    `INSERT INTO audit_log(id, actor, action, resource_type, resource_id, group_id, detail)
     SELECT $1, $2, 'agent.version_saved', 'agent', id, group_id, $3 FROM agents WHERE id = $4`,
    [randomUUID(), JSON.stringify({ id: input.actorId }), JSON.stringify({ version_id: id }), input.agentId],
  );
  return { id, version: result.rows[0]!.version };
}
