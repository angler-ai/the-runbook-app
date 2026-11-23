import { randomUUID } from 'node:crypto';
import type pg from 'pg';
import { DELIVERIES, type AgentCeiling, type AgentDirectoryEntry } from '@runbook/runner-shared/contracts/core';
import { withTransaction } from '@runbook/runner-shared/db';
import { CoreError, coreJson, type components } from '@runbook/runner-shared/core-client';
import { HttpError } from '@runbook/runner-shared/http';
import { findSecretShapedValues } from '@runbook/runner-shared/secret-shape';
import { readToolCalls, type ArchiveReader } from '../retention/read.ts';
import { authorize } from '../authorize.ts';
import type { PublicRequest } from '../server.ts';
import { loadCeiling, saveAgentVersionInTransaction, validateEgress, type AgentConfig } from './agents.ts';
import { applyEnvironment } from './environment-agent.ts';
import { createProposal } from './proposals.ts';

interface StoredAgent {
  id: string; name: string; description: string; group_id: string;
  status: 'draft' | AgentDirectoryEntry['status'];
  config: (AgentConfig & { instructions?: string; skills?: string[] }) | null;
  ceiling: AgentCeiling | null;
}

function denied(): never {
  throw new HttpError(403, 'auth.denied', 'runner resource unavailable');
}

function invalid(): never {
  throw new HttpError(422, 'agent.invalid', 'invalid agent configuration');
}

function absent(): never {
  throw new HttpError(404, 'agent.not_found', 'agent unavailable');
}

export interface SkillDirectory {
  coreUrl: string;
  installId: string;
  token: string;
  fetch?: typeof fetch;
}

async function pinSkills(directory: SkillDirectory | undefined, project: string, names: string[]) {
  if (!names.length) return [];
  if (!directory) throw new HttpError(503, 'agent.skills_unavailable', 'project skill library unavailable');
  const root = `/v1/runner-installs/${encodeURIComponent(directory.installId)}/projects/${encodeURIComponent(project)}/skills`;
  const pins = [];
  for (const name of new Set(names)) {
    try {
      pins.push(await coreJson<components['schemas']['RunnerSkillPin']>(
        { ...directory, baseUrl: directory.coreUrl }, 'GET', `${root}/${encodeURIComponent(name)}`,
      ));
    } catch (error) {
      if (error instanceof CoreError && error.status === 404)
        throw new HttpError(422, 'agent.skill_not_found', `project skill "${name}" was not found`);
      if (error instanceof CoreError && error.code === 'skill.too_large')
        throw new HttpError(422, 'agent.skills_too_large', 'agent skill pins exceed the prompt size limit');
      throw new HttpError(503, 'agent.skills_unavailable', 'project skill library unavailable');
    }
  }
  if (Buffer.byteLength(JSON.stringify(pins)) > 16 * 1024)
    throw new HttpError(422, 'agent.skills_too_large', 'agent skill pins exceed the prompt size limit');
  return pins;
}

export function agentResources(pool: pg.Pool, sync: () => Promise<void>, directory?: SkillDirectory, archives?: ArchiveReader) {
  return async ({ method, claims, body, target }: PublicRequest, projectId: string, id?: string, operation?: string, transaction?: pg.PoolClient) => {
    const resource = { kind: 'agent', projectId } as const;
    if (method === 'GET') {
      if (authorize(claims, 'read', resource) !== 'allow') denied();
      if (operation === 'versions') {
        const versions = await pool.query(
          `SELECT v.*, COALESCE(history.runs, '[]'::jsonb) AS runs,
             COALESCE(history.run_count, 0)::int AS run_count
           FROM agent_versions v JOIN agents a ON a.id = v.agent_id
           LEFT JOIN LATERAL (
             SELECT jsonb_agg(jsonb_build_object('execution_id', execution_id) ORDER BY last_attempt DESC, execution_id) AS runs,
               MAX(run_count) AS run_count
             FROM (
               SELECT execution_id, MAX(created_at) AS last_attempt, COUNT(*) OVER () AS run_count
               FROM slots WHERE agent_version_id = v.id AND agent_id = $1
                 AND project_id = $2 AND execution_id IS NOT NULL
               GROUP BY execution_id ORDER BY last_attempt DESC, execution_id LIMIT 50
             ) used
           ) history ON true
           WHERE a.id = $1 AND a.project_id = $2 ORDER BY v.version DESC LIMIT 1000`, [id, projectId],
        );
        return { body: { items: versions.rows } };
      }
      if (operation === 'tool-calls') {
        const step = new URL(target, 'http://internal').searchParams.get('step_run_id');
        return { body: await readToolCalls(pool, projectId, id ?? '', step, archives) };
      }
      const result = await pool.query(
        `SELECT a.id, a.project_id, a.group_id, a.name, a.description, a.current_version_id, a.created_at, a.updated_at,
           CASE WHEN a.status = 'active' AND NOT COALESCE(g.projects ? a.project_id, false)
             THEN 'paused' ELSE a.status END AS status, v.config, v.ceiling, v.version
         FROM agents a LEFT JOIN agent_versions v ON v.id = a.current_version_id
         LEFT JOIN groups g ON g.group_id = a.group_id
         WHERE a.project_id = $1 AND ($2::text IS NULL OR a.id = $2) ORDER BY a.name LIMIT 1000`, [projectId, id ?? null],
      );
      return id ? { body: result.rows[0] ?? absent() } : { body: { items: result.rows } };
    }

    if ((operation === 'pause' && method === 'POST') || method === 'DELETE') {
      if (authorize(claims, 'operate', resource) !== 'allow') denied();
      const state = method === 'DELETE' ? 'archived' : 'paused';
      await withTransaction(pool, async (client) => {
        const changed = await client.query('UPDATE agents SET status = $3, updated_at = now() WHERE id = $1 AND project_id = $2',
          [id, projectId, state]);
        if (!changed.rowCount) absent();
        await client.query(
          "INSERT INTO audit_log(id, actor, action, resource_type, resource_id) VALUES ($1,$2,$3,'agent',$4)",
          [randomUUID(), JSON.stringify({ kind: claims.principal, id: claims.user_id }), `agent.${state}`, id],
        );
      });
      await sync();
      return { status: 204 };
    }

    if (operation || !['POST', 'PATCH'].includes(method) || !body || typeof body !== 'object' || Array.isArray(body)) invalid();
    const input = body as Record<string, unknown>;
    const keys = Object.keys(input);
    if (keys.some((key) => !['name', 'description', 'group_id', 'status', 'instructions', 'skills',
      'profiles', 'credentials', 'limits', 'egress', 'mcp', 'sandbox', 'environment_id'].includes(key)) || findSecretShapedValues(input).length) invalid();
    const editableFields = claims.principal === 'human' ? ['description', 'instructions', 'skills'] : ['description'];
    const action = id && keys.every((key) => editableFields.includes(key)) ? 'edit' : 'widen';
    const access = authorize(claims, action, resource);
    if (access === 'deny') denied();
    if (access === 'proposal') {
      return { status: 202, body: await createProposal(pool, claims, {
        kind: 'agent.change', targetType: 'agent', targetId: id ?? null, diff: { ...input, project_id: projectId },
      }) };
    }

    const changesBudget = !id || 'limits' in input || 'environment_id' in input;
    if (changesBudget && (claims.principal !== 'human' || !claims.session_hash)) denied();

    const write = async (client: pg.PoolClient) => {
      const { rows: [current] } = id ? await client.query<StoredAgent>(
        `SELECT a.id, a.name, a.description, a.group_id, a.status, v.config, v.ceiling
         FROM agents a LEFT JOIN agent_versions v ON v.id = a.current_version_id
         WHERE a.id = $1 AND a.project_id = $2 FOR UPDATE OF a`, [id, projectId],
      ) : { rows: [] };
      if (id && !current) absent();
      const name = input.name ?? current?.name;
      const group = input.group_id ?? current?.group_id;
      const description = input.description ?? current?.description ?? '';
      const status = input.status ?? current?.status ?? 'draft';
      if (typeof name !== 'string' || !/^[a-z][a-z0-9_-]{0,127}$/.test(name) || name === 'external' ||
          typeof group !== 'string' || !group || typeof description !== 'string' ||
          !['draft', 'active', 'paused', 'archived'].includes(String(status))) invalid();
      if (group !== current?.group_id) {
        const allowed = await client.query('SELECT 1 FROM groups WHERE group_id = $1 AND projects ? $2', [group, projectId]);
        if (!allowed.rowCount) denied();
      }
      if ('limits' in input && (!input.limits || typeof input.limits !== 'object' || Array.isArray(input.limits))) invalid();
      const config = {
        profiles: input.profiles ?? current?.config?.profiles ?? [],
        credentials: input.credentials ?? current?.config?.credentials ?? [],
        limits: input.limits ?? current?.config?.limits ?? { max_cost_usd: 1, max_tokens: 16_384, timeout_s: 600 },
        egress: input.egress ?? current?.config?.egress ?? [],
        mcp: input.mcp ?? current?.config?.mcp ?? [],
        sandbox: input.sandbox === null ? undefined : input.sandbox ?? current?.config?.sandbox,
        instructions: input.instructions ?? current?.config?.instructions ?? '',
        skills: input.skills ?? current?.config?.skills ?? [],
      } as AgentConfig & { instructions: string; skills: string[] };
      if ((config.sandbox !== undefined && !['small', 'medium', 'large'].includes(config.sandbox?.size)) ||
          typeof config.instructions !== 'string' || !Array.isArray(config.skills) || config.skills.length > 16 ||
          config.skills.some((skill) => typeof skill !== 'string' || !/^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/.test(skill)) ||
          !Array.isArray(config.profiles) || config.profiles.some((profile) => typeof profile !== 'string') ||
          !Array.isArray(config.credentials) || config.credentials.some((entry) => !entry ||
            typeof entry.credential_id !== 'string' || !DELIVERIES.includes(entry.delivery)) ||
          !config.limits || typeof config.limits !== 'object' ||
          !Array.isArray(config.mcp) || config.mcp.some((entry) => !entry || typeof entry.server_id !== 'string' || typeof entry.tool !== 'string')) invalid();
      await applyEnvironment(client, claims.org_id, input, current?.config, config, action !== 'widen');
      config.egress = validateEgress(config.egress);
      let ceiling = current?.ceiling;
      if (action === 'widen' || !ceiling) {
        ceiling = await loadCeiling(client, config, projectId, claims.org_id);
      }
      config.skill_pins = await pinSkills(directory, projectId, config.skills);
      const agentId = current?.id ?? randomUUID();
      await client.query(
        `INSERT INTO agents(id,project_id,group_id,name,description,status) VALUES ($1,$2,$3,$4,$5,$6)
         ON CONFLICT (id) DO UPDATE SET group_id = EXCLUDED.group_id, name = EXCLUDED.name,
           description = EXCLUDED.description, status = EXCLUDED.status`, [agentId, projectId, group, name, description, status],
      );
      const version = await saveAgentVersionInTransaction(client, { agentId, projectId, actorId: claims.user_id, config, ceiling });
      return { id: agentId, version_id: version.id, version: version.version };
    };
    const saved = transaction ? await write(transaction) : await withTransaction(pool, write);
    if (!transaction) await sync();
    return { status: id ? 200 : 201, body: saved };
  };
}
