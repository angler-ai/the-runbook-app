import type pg from 'pg';
import { verifyAuditChain } from '@runbook/runner-shared/audit';

export async function exportAudit(pool: pg.Pool, after = '0') {
  const { rows } = await pool.query(
    `SELECT seq::text, id, at, actor, action, resource_type, resource_id, group_id, runner_id, detail,
       db_role, encode(prev_hash,'hex') AS prev_hash, encode(hash,'hex') AS hash
     FROM audit_log WHERE seq > $1 ORDER BY seq LIMIT 1000`, [after],
  );
  const chain = await verifyAuditChain(pool);
  return { chain, items: rows };
}

export async function agentAudit(pool: pg.Pool, project: string, agent: string, after: string) {
  const { rows } = await pool.query(
    `SELECT l.seq::text, l.id, l.at, l.actor, l.action, l.resource_type, l.resource_id, l.detail
     FROM audit_log l JOIN agents a ON a.id = l.resource_id
     WHERE a.project_id = $1 AND a.id = $2 AND l.resource_type = 'agent' AND l.seq > $3
     ORDER BY l.seq LIMIT 1000`, [project, agent, after],
  );
  return { items: rows };
}
