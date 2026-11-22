import type pg from 'pg';
import type { Config } from '@runbook/runner-shared/config';
import { matchPublicRoute } from '@runbook/runner-shared/contracts/public';
import { HttpError } from '@runbook/runner-shared/http';
import { authorize } from '../authorize.ts';
import type { PublicRequest } from '../server.ts';
import { agentResources, type SkillDirectory } from './agent-routes.ts';
import { agentActivity } from './activity.ts';
import { credentialResources } from './credentials.ts';
import { catalogPresets, catalogResources, isCatalog } from './catalog.ts';
import { proposalResources } from './proposals.ts';
import { agentAudit, exportAudit } from './audit.ts';
import { readTranscript } from './transcripts.ts';
import { readUsage, usageWindow } from './usage.ts';

function denied(): never {
  throw new HttpError(403, 'auth.denied', 'runner resource unavailable');
}

export function publicResources(pool: pg.Pool, config: Config, sync: () => Promise<void>, directory?: SkillDirectory) {
  const agents = agentResources(pool, sync, directory);
  const catalog = catalogResources(pool, config, sync);
  const credentials = credentialResources(pool, config, sync);
  const proposals = proposalResources(pool, async (request, proposal, client) => {
    const method = proposal.target_id ? 'PATCH' : 'POST';
    if (proposal.kind === 'agent.change') {
      const { project_id, ...body } = proposal.diff;
      return agents({ ...request, method, body }, String(project_id), proposal.target_id ?? undefined, undefined, client);
    }
    if (proposal.kind === 'catalog.change') {
      const { resource, change } = proposal.diff;
      if (typeof resource !== 'string' || !isCatalog(resource)) throw new HttpError(422, 'proposal.invalid', 'invalid proposal');
      return catalog({ ...request, method: resource === 'policy' ? 'PUT' : method, body: change },
        resource, proposal.target_id ?? undefined, undefined, client);
    }
    throw new HttpError(422, 'proposal.invalid', 'invalid proposal');
  }, sync);
  return async (request: PublicRequest): Promise<{ status?: number; body?: unknown }> => {
    const { method, target, claims } = request;
    const url = new URL(target, 'http://internal');
    const route = matchPublicRoute(method, url.pathname);
    if (!route) throw new HttpError(404, 'route.not_found', 'no such route');
    if (!claims.scopes.includes(route.scope)) denied();
    if (method === 'GET' && url.pathname === '/runner/v1/usage') {
      const admin = claims.org_role === 'owner' || claims.org_role === 'admin';
      const window = usageWindow(url.searchParams.get('level') ?? 'project', url.searchParams.get('period') ?? 'day',
        url.searchParams.get('date') ?? new Date().toISOString().slice(0, 10));
      return { body: { items: await readUsage(pool, window, admin ? null : Object.keys(claims.project_roles)) } };
    }
    if (method === 'GET' && url.pathname === '/runner/v1/audit') {
      if (claims.org_role !== 'owner' && claims.org_role !== 'admin') denied();
      const cursor = url.searchParams.get('after') ?? '0';
      if (!/^\d{1,18}$/.test(cursor)) throw new HttpError(400, 'audit.invalid_cursor', 'invalid audit cursor');
      return { body: await exportAudit(pool, cursor) };
    }
    const segments = url.pathname.split('/');
    const resource = segments[3];
    if (method === 'GET' && !segments[4] && url.searchParams.get('presets') === 'true' &&
        (resource === 'credentials' || resource === 'mcp-servers')) return catalogPresets(resource, claims);
    if (isCatalog(resource)) return catalog(request, resource, segments[4], segments[5]);
    if (resource === 'proposals') {
      const id = segments[4];
      if (id && method === 'POST' && segments[5] === 'approve') {
        const { rows: [proposal] } = await pool.query<{ kind: string }>('SELECT kind FROM proposals WHERE id = $1', [id]);
        if (proposal?.kind === 'credential.change') return credentials.approve(request, id);
      }
      return proposals(request, id, segments[5]);
    }
    if (segments[3] === 'credentials' || segments[3] === 'ingest-key') return credentials(request, segments);
    if (method === 'GET' && route.path === '/runner/v1/transcripts/{step_run_id}') {
      const cursor = Number(url.searchParams.get('after') ?? '0');
      if (!Number.isSafeInteger(cursor) || cursor < 0) throw new HttpError(400, 'transcript.invalid_cursor', 'invalid transcript cursor');
      return { body: { items: await readTranscript(pool, claims, segments[4] ?? '', cursor, url.searchParams.get('claim_id') ?? undefined) } };
    }
    if (method === 'GET' && route.path === '/runner/v1/projects/{project_id}/agents/{agent_id}/audit') {
      const projectId = segments[4] ?? '';
      if (!Object.hasOwn(claims.project_roles, projectId) || claims.project_roles[projectId] !== 'admin') denied();
      const cursor = url.searchParams.get('after') ?? '0';
      if (!/^\d{1,18}$/.test(cursor)) throw new HttpError(400, 'audit.invalid_cursor', 'invalid audit cursor');
      return { body: await agentAudit(pool, projectId, segments[6] ?? '', cursor) };
    }
    if (method === 'GET' && route.path === '/runner/v1/projects/{project_id}/agents/{agent_id}/activity') {
      const projectId = segments[4] ?? '';
      if (authorize(claims, 'read', { kind: 'agent', projectId }) !== 'allow') denied();
      const now = Date.now();
      return { body: await agentActivity(pool, projectId, segments[6] ?? '', new Date(now - 86_400_000), new Date(now)) };
    }
    if (segments[3] === 'projects' && segments[5] === 'agents') {
      return agents(request, segments[4] ?? '', segments[6], segments[7]);
    }
    throw new HttpError(501, 'route.unavailable', 'runner route unavailable');
  };
}
