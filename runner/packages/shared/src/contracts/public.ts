// Shared contracts: the runner's public API, served by runner-api under
// /runner/v1 and reached only through the core proxy
// (`/v1/runners/{ri}/proxy/{*path}`, Task R0-5). The core's path allow-list
// is generated from this file: `npm run gen:routes` writes
// public-routes.json, which the core embeds as runner_routes.json.

export const PUBLIC_PREFIX = '/runner/v1';

export type PublicMethod = 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE';

export interface PublicRoute {
  method: PublicMethod;
  /** Path template under the prefix, with `{name}` segments. */
  path: string;
  /** The runner scope the proxy requires: reads need `runner:read`, writes `runner:admin`. */
  scope: 'runner:read' | 'runner:admin';
  sensitive: boolean;
  mfa_reason: string;
  /** The response may be `text/event-stream` (only the transcript route). */
  sse?: true;
  core_only?: true;
}

type MfaPolicy = Pick<PublicRoute, 'sensitive' | 'mfa_reason'>;
const freshMfa = (mfa_reason: string): MfaPolicy => ({ sensitive: true, mfa_reason });
const READ: MfaPolicy = { sensitive: false, mfa_reason: 'Read under runner authorization' };
const PROPOSAL_EDIT: MfaPolicy = { sensitive: false, mfa_reason: 'Edits a proposal without applying it' };

interface Resource {
  /** Collection path, e.g. `/groups`. */
  base: string;
  /** Path parameter of one item. */
  id: string;
  /** Fresh MFA for configuration mutations on this resource. */
  writeMfa: MfaPolicy;
  /** Default: list, get, create, update, delete. */
  ops?: readonly Op[];
  /** Extra routes under the item: `[method, suffix, mfa]`. */
  item?: readonly (readonly [PublicMethod, string, MfaPolicy])[];
}

type Op = 'list' | 'get' | 'create' | 'update' | 'delete';
const CRUD: readonly Op[] = ['list', 'get', 'create', 'update', 'delete'];

/** The plan's resource list ("Runner public API"), in its order. */
const RESOURCES: readonly Resource[] = [
  { base: '/groups', id: 'group_id', writeMfa: freshMfa('Changes runner group configuration'), },
  { base: '/providers', id: 'provider_id', writeMfa: freshMfa('Changes model provider configuration'), },
  { base: '/profiles', id: 'profile_id', writeMfa: freshMfa('Changes runner profile configuration'), },
  {
    base: '/mcp-servers',
    id: 'server_id', writeMfa: freshMfa('Changes upstream MCP configuration'),
    item: [
      ['POST', '/discover', freshMfa('Contacts the configured upstream MCP server')],
      ['POST', '/pin', freshMfa('Changes the trusted upstream tool snapshot')],
      ['GET', '/tools', READ],
      ['GET', '/drift', READ],
    ],
  },
  {
    base: '/credentials',
    id: 'credential_id', writeMfa: freshMfa('Changes runner credentials'),
    item: [
      ['POST', '/secrets/{field}', freshMfa('Changes a runner secret')],
      ['POST', '/test', freshMfa('Exercises a runner credential')],
      ['POST', '/revoke', freshMfa('Revokes a runner credential')],
      ['POST', '/oauth-discover', freshMfa('Discovers the OAuth issuer and endpoints')],
      ['POST', '/oauth-confirm', freshMfa('Confirms the OAuth endpoints and registers a client')],
      ['POST', '/connect', freshMfa('Starts an OAuth authorization under a full human session')],
      ['POST', '/disconnect', freshMfa('Disconnects an OAuth credential')],
    ],
  },
  {
    base: '/projects/{project_id}/agents',
    id: 'agent_id', writeMfa: freshMfa('Changes registered agent configuration'),
    item: [
      ['GET', '/versions', READ],
      ['GET', '/audit', READ],
      ['POST', '/pause', freshMfa('Changes registered agent availability')],
      ['GET', '/activity', READ],
      ['GET', '/tool-calls', READ],
    ],
  },
  {
    base: '/proposals',
    id: 'proposal_id', writeMfa: PROPOSAL_EDIT,
    item: [
      ['POST', '/approve', freshMfa('Authorizes a proposed change')],
      ['POST', '/reject', PROPOSAL_EDIT],
    ],
  },
];

function scopeOf(method: PublicMethod): PublicRoute['scope'] {
  return method === 'GET' ? 'runner:read' : 'runner:admin';
}

function route(method: PublicMethod, path: string, mfa: MfaPolicy, sse?: true): PublicRoute {
  return { method, path, scope: scopeOf(method), ...mfa, ...(sse ? { sse } : {}) };
}

function expand(resource: Resource): PublicRoute[] {
  const item = `${resource.base}/{${resource.id}}`;
  const routes: PublicRoute[] = [];
  for (const op of resource.ops ?? CRUD) {
    switch (op) {
      case 'list':
        routes.push(route('GET', resource.base, READ));
        break;
      case 'get':
        routes.push(route('GET', item, READ));
        break;
      case 'create':
        routes.push(route('POST', resource.base, resource.writeMfa));
        break;
      case 'update':
        routes.push(route('PATCH', item, resource.writeMfa));
        break;
      case 'delete':
        routes.push(route('DELETE', item, resource.writeMfa));
        break;
    }
  }
  for (const [method, suffix, mfa] of resource.item ?? []) routes.push(route(method, item + suffix, mfa));
  return routes;
}

/** Every route of the runner's public API, in a stable order. */
export const PUBLIC_ROUTES: readonly PublicRoute[] = [
  ...RESOURCES.slice(0, 5).flatMap(expand),
  { ...route('POST', '/oauth/callback/{credential_id}', { sensitive: false, mfa_reason: 'Completes a browser authorization after step-up at connect' }), core_only: true },
  route('GET', '/ingest-key', READ),
  route('GET', '/policy', READ),
  route('PUT', '/policy', freshMfa('Changes runner policy')),
  ...RESOURCES.slice(5).flatMap(expand),
  ...expand({
    base: '/environments', id: 'environment_id', writeMfa: freshMfa('Changes sandbox environment configuration'),
    item: [['POST', '/resolve-digest', freshMfa('Resolves and pins a sandbox image digest')]],
  }),
  route('GET', '/environment-registries', READ),
  route('PUT', '/environment-registries', freshMfa('Changes allowed sandbox image registries')),
  route('GET', '/projects/{project_id}/mcp-tools', READ),
  route('GET', '/budgets', READ),
  route('GET', '/usage', READ),
  route('GET', '/transcripts/{step_run_id}', READ, true),
  route('GET', '/audit', READ),
];

/** The JSON document the core embeds (`crates/runbook-api/src/runner_routes.json`). */
export interface PublicRoutesDocument {
  version: 1;
  prefix: string;
  routes: PublicRoute[];
}

export function publicRoutesDocument(): PublicRoutesDocument {
  return {
    version: 1,
    prefix: PUBLIC_PREFIX,
    routes: PUBLIC_ROUTES.map((r) => ({ ...r, path: PUBLIC_PREFIX + r.path })),
  };
}

/** The document as the checked-in file spells it (two-space JSON, trailing newline). */
export function renderPublicRoutes(): string {
  return JSON.stringify(publicRoutesDocument(), null, 2) + '\n';
}

/**
 * Match a concrete path against the route list, as the core proxy does: exact
 * segments, `{name}` matching one non-empty segment without `..`.
 */
export function matchPublicRoute(method: string, path: string): PublicRoute | undefined {
  const segments = path.split('/');
  return publicRoutesDocument().routes.find((r) => {
    if (r.method !== method) return false;
    const want = r.path.split('/');
    if (want.length !== segments.length) return false;
    return want.every((w, i) => {
      const got = segments[i] ?? '';
      if (w.startsWith('{') && w.endsWith('}')) return got !== '' && got !== '.' && got !== '..';
      return w === got;
    });
  });
}
