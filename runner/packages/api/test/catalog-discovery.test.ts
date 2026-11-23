import { createHash } from 'node:crypto';
import type { AddressInfo } from 'node:net';
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { canonicalJson } from '@runbook/runner-shared/binding';
import type { UserAssertionClaims } from '@runbook/runner-shared/contracts/core';
import { captureLogs } from '@runbook/runner-shared/log';
import { createTestDatabase, hasTestDatabase, type TestDatabase } from '@runbook/runner-shared/test-utils/pg';
import { catalogResources } from '../src/resources/catalog.ts';
import { createApiServer } from '../src/server.ts';
import { githubTools, linearTools, notionTools } from './fixtures/mcp-tool-lists.ts';

const claims: UserAssertionClaims = {
  aud: 'runner:install', iss: 'core', jti: 'test', iat: 0, exp: 60, org_id: 'org', user_id: 'human',
  org_role: 'admin', principal: 'human', project_roles: {}, scopes: ['runner:admin', 'runner:read'],
  htm: 'POST', htu: '', bh: '',
};
const actualFetch = globalThis.fetch;
const endpoint = 'https://mcp.notion.com/mcp';

describe.skipIf(!hasTestDatabase)('MCP catalog discovery', () => {
  let db: TestDatabase;
  let server: ReturnType<typeof createApiServer>;
  let baseUrl: string;
  const outbound = vi.fn<typeof fetch>();

  beforeAll(async () => {
    db = await createTestDatabase();
    const catalog = catalogResources(db.pool('api'), {
      role: 'api', listen: { host: '', port: 0 }, urls: { vault: 'https://vault.example' },
      inboundTokens: {}, outboundTokens: { vault: 'vault-process-token' },
    }, async () => {});
    server = createApiServer({ verifyAssertion: async () => claims }, (request) => {
      const [, , , , id, operation] = request.target.split('/');
      return catalog(request, 'mcp-servers', id, operation);
    });
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    baseUrl = `http://127.0.0.1:${(server.address() as AddressInfo).port}/runner/v1/mcp-servers`;
    await db.admin().query(
      `INSERT INTO credentials(id, org_id, name, type, status, binding)
       VALUES ('notion-connection', 'org', 'Notion', 'mcp-oauth', 'active', $1)`, [JSON.stringify({ url: endpoint })],
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    outbound.mockReset();
  });

  afterAll(async () => {
    if (server) await new Promise<void>((resolve) => server.close(() => resolve()));
    await db?.drop();
  });

  function request(path: string, method: string, body?: unknown) {
    return actualFetch(baseUrl + path, {
      method,
      headers: { 'runbook-assertion': 'fixture-assertion', 'content-type': 'application/json' },
      body: JSON.stringify(body),
    });
  }

  async function addServer(name: string, oauth = false) {
    const response = await request('', 'POST', {
      name,
      url: oauth ? endpoint : `https://${name}.example/mcp`,
      transport: 'streamable-http',
      ...(oauth && { credential_id: 'notion-connection' }),
    });
    expect(response.status).toBe(201);
    return (await response.json() as { id: string }).id;
  }

  it.each([
    { provider: 'Notion OAuth', name: 'notion', tools: notionTools, oauth: true },
    { provider: 'GitHub', name: 'github', tools: githubTools, oauth: false },
    { provider: 'Linear', name: 'linear', tools: linearTools, oauth: false },
  ])('discovers and pins the $provider tool list with its canonical hash', async ({ name, tools, oauth }) => {
    if (oauth) {
      expect(tools).toHaveLength(77);
      expect(Buffer.byteLength(JSON.stringify(tools))).toBe(232_228);
    }
    const id = await addServer(name, oauth);
    outbound.mockResolvedValue(Response.json({ tools }));
    vi.stubGlobal('fetch', outbound);
    const discovered = await request(`/${id}/discover`, 'POST');
    expect(discovered.status).toBe(200);
    const snapshot = await discovered.json() as { snapshot_id: string };
    const hash = createHash('sha256').update(canonicalJson(tools)).digest('hex');
    expect(snapshot).toEqual({ snapshot_id: expect.any(String), hash, tools });
    if (oauth) {
      expect(outbound).toHaveBeenCalledWith(`https://vault.example/internal/mcp/${id}/oauth-discover`, expect.objectContaining({
        headers: expect.objectContaining({ 'runbook-assertion': 'fixture-assertion' }),
      }));
    }
    const pinned = await request(`/${id}/pin`, 'POST', { snapshot_id: snapshot.snapshot_id });
    expect(pinned.status).toBe(200);
    expect(await pinned.json()).toMatchObject({ pinned_snapshot_id: snapshot.snapshot_id });
    const { rows } = await db.admin().query(
      "SELECT encode(hash, 'hex') AS hash, tools FROM mcp_tool_snapshots WHERE id = $1", [snapshot.snapshot_id],
    );
    expect(rows).toEqual([{ hash, tools }]);
  });

  const validTool = { name: 'read', inputSchema: { type: 'object' } };
  it.each([
    { name: 'missing-list', tools: undefined, reason: 'MCP discovery response must contain a tools array' },
    { name: 'tool-count', tools: Array(501).fill(validTool) as unknown[], reason: 'MCP tool list exceeds the 500-tool limit (501)' },
    { name: 'tool-bytes', tools: [{ ...validTool, description: 'é'.repeat(524_288) }],
      reason: 'MCP tool list exceeds the 1048576-byte limit (1048642 bytes)' },
    { name: 'null-tool', tools: [null], reason: 'MCP tool #1 must be an object' },
    { name: 'invalid-name', tools: [{ ...validTool, name: ' '.repeat(129) }], reason: 'MCP tool #1 name must be nonblank and at most 128 characters' },
    { name: 'duplicate-name', tools: [validTool, validTool], reason: 'MCP tool #2 duplicates the name of tool #1' },
    { name: 'invalid-input', tools: [validTool, { name: 'write', inputSchema: { type: 'array' } }],
      reason: 'MCP tool #2 inputSchema must be a JSON Schema object with type "object"' },
  ])('returns and logs a precise 422 for $name discovery', async ({ name, tools, reason }) => {
    const id = await addServer(name);
    outbound.mockResolvedValue(Response.json({ tools }));
    vi.stubGlobal('fetch', outbound);
    const logs = captureLogs();
    try {
      const response = await request(`/${id}/discover`, 'POST');
      expect(response.status).toBe(422);
      expect(await response.json()).toEqual({ error: { code: 'catalog.invalid_tool_list', message: reason } });
      expect(logs.lines.map((line) => JSON.parse(line) as unknown)).toContainEqual(expect.objectContaining({
        level: 'warn', server_id: id, reason,
      }));
      const { rowCount } = await db.admin().query('SELECT 1 FROM mcp_tool_snapshots WHERE server_id = $1', [id]);
      expect(rowCount).toBe(0);
    } finally {
      logs.restore();
    }
  });

  it('returns 502 when the vault is unavailable', async () => {
    const id = await addServer('vault-unavailable');
    outbound.mockResolvedValue(new Response(null, { status: 503 }));
    vi.stubGlobal('fetch', outbound);
    const response = await request(`/${id}/discover`, 'POST');
    expect(response.status).toBe(502);
    expect(await response.json()).toMatchObject({ error: { code: 'catalog.discovery_unavailable' } });
  });
});
