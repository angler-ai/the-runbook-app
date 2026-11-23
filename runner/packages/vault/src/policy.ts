// Runner policy for a release (design §7.5, rule 1), read by the vault from
// runner-api's configuration before it asks the core for anything: the
// credential is shared with the step's project, listed in the agent version
// pinned at claim with that delivery mode, allowed in the step's runner
// group, supported by its type, and not revoked, and the agent is active.
// (The group check is then enforced cryptographically by the binding at
// unwrap.)
import type pg from 'pg';
import type { Delivery, GrantClaims } from '@runbook/runner-shared/contracts/core';
import { VaultError } from './errors.ts';
import type { CredentialRow } from './secrets.ts';
import { canonicalGroups, groupAllowed, typeImpl } from './types/index.ts';

export const notAllowed = () =>
  new VaultError(403, 'release.not_allowed', 'the agent version may not use this credential this way');

/** Sharing defaults to none: a project sees only what is shared with it. */
export function sharedWith(sharing: unknown, projectId: string): boolean {
  const projects = (sharing as { projects?: unknown } | null)?.projects;
  if (projects === 'all') return true;
  return Array.isArray(projects) && projects.includes(projectId);
}

interface AgentVersionRow {
  agent_id: string;
  project_id: string;
  group_id: string;
  status: string;
  config: { profiles?: string[]; credentials?: { credential_id?: unknown; delivery?: unknown }[] };
}

export async function checkRunnerPolicy(
  pool: pg.Pool | pg.PoolClient,
  capability: Pick<GrantClaims, 'project_id' | 'group_id' | 'agent_id' | 'agent_version' | 'connections' | 'destructive' | 'reach' | 'model_profile'>,
  credential: CredentialRow,
  delivery: Delivery,
): Promise<void> {
  if ((capability.reach === 'read' && (credential.label !== 'read' || !capability.connections?.includes(credential.id))) ||
      (delivery !== 'model' && capability.connections !== null && !capability.connections.includes(credential.id)) ||
      (!capability.destructive && credential.label !== 'read' && credential.label !== 'write')) throw notAllowed();
  if (delivery === 'token') {
    throw new VaultError(403, 'release.token_delivery_disabled', 'token delivery is unavailable until the vault supports it');
  }
  const { rows } = await pool.query<AgentVersionRow>(
    `SELECT v.agent_id, a.project_id, a.group_id, a.status, v.config
     FROM agent_versions v JOIN agents a ON a.id = v.agent_id
     WHERE v.id = $1 AND v.agent_id = $2`,
    [capability.agent_version, capability.agent_id],
  );
  const version = rows[0];
  if (!version) throw notAllowed();
  if (version.project_id !== capability.project_id || version.group_id !== capability.group_id) throw notAllowed();
  // Only an active agent gets credentials: draft, paused and archived are refused.
  if (version.status !== 'active') throw notAllowed();
  if (delivery === 'model') {
    const { rowCount } = await pool.query(
      `SELECT 1 FROM profiles p JOIN providers v ON v.id = p.provider_id
       WHERE v.credential_id = $1 AND p.id = ANY($2::text[])
         AND ($3::text IS NULL OR p.name = $3)
         AND (p.sharing->>'projects' = 'all' OR p.sharing->'projects' ? $4)
         AND (v.sharing->>'projects' = 'all' OR v.sharing->'projects' ? $4)`,
      [credential.id, version.config.profiles ?? [], capability.model_profile, capability.project_id],
    );
    if (!rowCount) throw notAllowed();
  } else if (!(version.config.credentials ?? []).some(
    (c) => c.credential_id === credential.id && c.delivery === delivery,
  )) throw notAllowed();
  if (credential.status === 'revoked') throw new VaultError(403, 'release.not_allowed', 'the credential is revoked');
  if (!sharedWith(credential.sharing, capability.project_id)) throw notAllowed();
  if (!typeImpl(credential.type).deliveries.includes(delivery)) throw notAllowed();
  let groups;
  try {
    groups = canonicalGroups(credential.groups);
  } catch {
    throw notAllowed();
  }
  if (!groupAllowed(groups, capability.group_id)) throw notAllowed();
}
