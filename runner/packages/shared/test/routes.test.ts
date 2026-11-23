import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { matchPublicRoute, PUBLIC_ROUTES, renderPublicRoutes } from '../src/contracts/public.ts';

const file = new URL('../src/contracts/public-routes.json', import.meta.url);

describe('public routes', () => {
  it('routes_json_matches_public_contract', () => {
    // `npm run gen:routes` regenerates the file; the core embeds a copy.
    expect(readFileSync(file, 'utf8')).toBe(renderPublicRoutes());
  });

  it('covers every resource in the plan', () => {
    const doc = JSON.parse(readFileSync(file, 'utf8')) as {
      prefix: string;
      routes: { method: string; path: string; scope: string; sse?: boolean }[];
    };
    expect(doc.prefix).toBe('/runner/v1');
    const keys = new Set(doc.routes.map((r) => `${r.method} ${r.path}`));
    for (const want of [
      'GET /runner/v1/groups',
      'DELETE /runner/v1/providers/{provider_id}',
      'PATCH /runner/v1/profiles/{profile_id}',
      'POST /runner/v1/mcp-servers/{server_id}/discover',
      'POST /runner/v1/mcp-servers/{server_id}/pin',
      'GET /runner/v1/mcp-servers/{server_id}/tools',
      'GET /runner/v1/mcp-servers/{server_id}/drift',
      'GET /runner/v1/projects/{project_id}/mcp-tools',
      'POST /runner/v1/credentials/{credential_id}/secrets/{field}',
      'POST /runner/v1/credentials/{credential_id}/test',
      'POST /runner/v1/credentials/{credential_id}/revoke',
      'GET /runner/v1/ingest-key',
      'GET /runner/v1/policy',
      'PUT /runner/v1/policy',
      'GET /runner/v1/projects/{project_id}/agents',
      'GET /runner/v1/projects/{project_id}/agents/{agent_id}/versions',
      'POST /runner/v1/projects/{project_id}/agents/{agent_id}/pause',
      'GET /runner/v1/projects/{project_id}/agents/{agent_id}/activity',
      'GET /runner/v1/projects/{project_id}/agents/{agent_id}/tool-calls',
      'POST /runner/v1/proposals/{proposal_id}/approve',
      'POST /runner/v1/proposals/{proposal_id}/reject',
      'GET /runner/v1/usage',
      'GET /runner/v1/transcripts/{step_run_id}',
      'GET /runner/v1/audit',
    ]) {
      expect(keys.has(want), want).toBe(true);
    }
    // No route twice; reads need runner:read, writes runner:admin; SSE only on transcripts.
    expect(keys.size).toBe(doc.routes.length);
    for (const r of doc.routes) {
      expect(r.scope).toBe(r.method === 'GET' ? 'runner:read' : 'runner:admin');
      expect(Boolean(r.sse)).toBe(r.path === '/runner/v1/transcripts/{step_run_id}');
    }
    // The secret route takes a sealed blob; nothing ever reads a secret back.
    expect(PUBLIC_ROUTES.some((r) => r.method === 'GET' && r.path.includes('/secrets'))).toBe(false);
  });

  it('matches concrete paths like the core proxy', () => {
    expect(matchPublicRoute('POST', '/runner/v1/credentials/cr_1/secrets/key')?.scope).toBe('runner:admin');
    expect(matchPublicRoute('GET', '/runner/v1/credentials/cr_1/secrets/key')).toBeUndefined();
    expect(matchPublicRoute('GET', '/runner/v1/credentials/../policy')).toBeUndefined();
    expect(matchPublicRoute('GET', '/runner/v1/internal/seal')).toBeUndefined();
  });
});
