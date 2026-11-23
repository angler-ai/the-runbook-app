import type pg from 'pg';
import { loadCeiling, type AgentConfig } from './resources/agents.ts';
import type { AgentDirectoryEntry } from '@runbook/runner-shared/contracts/core';

export async function syncAgent(options: { coreUrl: string; installId: string; token: string; fetch?: typeof fetch },
  entry: AgentDirectoryEntry) {
  const response = await (options.fetch ?? fetch)(
    `${options.coreUrl}/v1/runner-installs/${encodeURIComponent(options.installId)}/agents/${encodeURIComponent(entry.agent_id)}`, {
      method: 'PUT', headers: { authorization: `Bearer ${options.token}`, 'content-type': 'application/json' },
      body: JSON.stringify(entry), redirect: 'error', signal: AbortSignal.timeout(10_000),
    });
  if (!response.ok) throw new Error('runner agent directory sync failed');
}

type DirectoryOptions = Parameters<typeof syncAgent>[0] & { orgId: string };

export async function syncGroupProjects(pool: pg.Pool, options: DirectoryOptions): Promise<void> {
  const response = await (options.fetch ?? fetch)(
    `${options.coreUrl}/v1/runner-installs/${encodeURIComponent(options.installId)}/groups`, {
      headers: { authorization: `Bearer ${options.token}` },
      redirect: 'error', signal: AbortSignal.timeout(10_000),
    });
  if (!response.ok) throw new Error('runner group registry unavailable');
  const status = await response.json() as { install_id?: string; groups?: { id: string; projects: string[] }[] } | null;
  if (status?.install_id !== options.installId || !Array.isArray(status.groups) ||
      status.groups.some((group) => !group || typeof group.id !== 'string' || !Array.isArray(group.projects) ||
        group.projects.some((project) => typeof project !== 'string')) ||
      new Set(status.groups.map(({ id }) => id)).size !== status.groups.length) {
    throw new Error('invalid runner group registry');
  }
  await pool.query(
    `UPDATE groups g SET projects = snapshot.projects, updated_at = now()
     FROM (
       SELECT local.group_id, COALESCE(remote.projects, '[]'::jsonb) AS projects
       FROM groups local LEFT JOIN jsonb_to_recordset($1::jsonb) AS remote(id text, projects jsonb)
         ON remote.id = local.group_id
     ) snapshot
     WHERE g.group_id = snapshot.group_id AND g.projects IS DISTINCT FROM snapshot.projects`,
    [JSON.stringify(status.groups)],
  );
}

export async function syncDirectory(pool: pg.Pool, options: DirectoryOptions): Promise<void> {
  await syncGroupProjects(pool, options);
  const { rows } = await pool.query<Omit<AgentDirectoryEntry, 'ceiling'> & { config: AgentConfig }>(
    `SELECT a.id AS agent_id, a.name, a.project_id, a.group_id, a.status, v.id AS version_id, v.config
     FROM agents a JOIN agent_versions v ON v.id = a.current_version_id
     JOIN groups g ON g.group_id = a.group_id AND g.projects ? a.project_id
     WHERE a.status <> 'draft'
     ORDER BY a.id`,
  );
  const results = await Promise.allSettled(rows.map(async ({ config, ...entry }) => {
    const ceiling = await loadCeiling(pool, config, entry.project_id, options.orgId, true);
    await syncAgent(options, { ...entry, ceiling });
  }));
  if (results.some(({ status }) => status === 'rejected')) throw new Error('runner agent directory sync failed');
}
