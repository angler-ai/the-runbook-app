import { expect, it } from 'vitest';
import { discoverOAuth, tokenPair } from '../src/oauth/provider.ts';

const url = 'https://mcp.example/tenant/mcp';
const issuer = 'https://auth.example/issuer';
const input = { url, registration_mode: 'dynamic' };
const paths = [
  url,
  'https://mcp.example/.well-known/oauth-protected-resource/tenant/mcp',
  'https://mcp.example/.well-known/oauth-protected-resource',
  'https://auth.example/.well-known/oauth-authorization-server/issuer',
  'https://auth.example/.well-known/openid-configuration/issuer',
  'https://auth.example/issuer/.well-known/openid-configuration',
];

function provider(override: Record<string, unknown> = {}, resource = 'https://mcp.example') {
  const calls: string[] = [];
  const checked: string[] = [];
  return {
    calls, checked,
    async check(destination: string) { checked.push(destination); },
    async request(destination: string) {
      calls.push(destination);
      if (destination === url) return { status: 401, headers: new Headers(), body: null };
      if (destination === paths[2]) return {
        status: 200, headers: new Headers(), body: { resource, authorization_servers: [issuer] },
      };
      if (destination === paths[5]) return { status: 200, headers: new Headers(), body: {
        issuer, authorization_endpoint: 'https://auth.example/authorize', token_endpoint: 'https://auth.example/token',
        registration_endpoint: 'https://auth.example/register', revocation_endpoint: 'https://revoke.example/token',
        code_challenge_methods_supported: ['S256'], authorization_response_iss_parameter_supported: true, ...override,
      } };
      return { status: 404, headers: new Headers(), body: null };
    },
  };
}

it('discovers path-aware metadata in protocol order and checks every endpoint', async () => {
  const mock = provider();
  await expect(discoverOAuth(mock, input)).resolves.toMatchObject({
    url, issuer, resource: 'https://mcp.example', iss_supported: true, token_endpoint_auth_method: 'none',
  });
  expect(mock.calls).toEqual(paths);
  expect(mock.checked).toEqual(['https://auth.example/authorize', 'https://auth.example/token', 'https://revoke.example/token', 'https://auth.example/register']);
});

it.each([
  [{ issuer: paths[5] }, 'https://mcp.example'],
  [{ code_challenge_methods_supported: ['plain'] }, 'https://mcp.example'],
  [{}, 'https://mcp.example/ten'],
  [{}, 'https://elsewhere.example/tenant/mcp'],
])('rejects inconsistent resource or issuer metadata', async (override, resource) => {
  await expect(discoverOAuth(provider(override, resource), input)).rejects.toThrow();
});

it('rejects credential collisions and suppresses reflected submitted grants in display data', () => {
  const response = (body: object) => ({ status: 200, headers: new Headers(), body: {
    token_type: 'Bearer', access_token: 'access-value', ...body,
  } });
  expect(() => tokenPair(response({ refresh_token: 'access-value' }), new Date(), []))
    .toThrow(/colliding credentials/);
  expect(() => tokenPair(response({ access_token: 'client-value' }), new Date(), [], 'client-value'))
    .toThrow(/colliding credentials/);
  expect(() => tokenPair(response({ access_token: 'submitted-code' }), new Date(), [], undefined, 'submitted-code'))
    .toThrow(/colliding credentials/);
  expect(tokenPair(response({ connected_account: 'account prior-refresh' }), new Date(), [],
    undefined, 'prior-refresh', true).account).toBeUndefined();
});

it('classifies a refused CONNECT as a request that did not reach the provider', async () => {
  const { createServer } = await import('node:http');
  const { TunneledOAuthProvider } = await import('../src/oauth/provider.ts');
  const gateway = createServer().on('connect', (_req, socket) => {
    socket.end('HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n');
  });
  await new Promise<void>((resolve) => gateway.listen(0, '127.0.0.1', resolve));
  try {
    const address = gateway.address();
    if (!address || typeof address === 'string') throw new Error('missing gateway address');
    const provider = new TunneledOAuthProvider(`http://127.0.0.1:${address.port}`, 'fixture');
    await expect(provider.request('https://provider.example/token', {
      method: 'POST', headers: {}, body: 'fixture',
    })).rejects.toMatchObject({ code: 'oauth.provider_unavailable', sent: false });
  } finally {
    await new Promise<void>((resolve) => gateway.close(() => resolve()));
  }
});

it.each([{ email: 'reviewer@example.test', sub: 'opaque-42' }, { sub: 'opaque-42' }])(
  'uses account claims from the bound token endpoint when identity is otherwise unavailable (%j)', (claims) => {
    const id_token = `header.${Buffer.from(JSON.stringify(claims)).toString('base64url')}.signature`;
    const result = tokenPair({ status: 200, headers: new Headers(),
      body: { token_type: 'Bearer', access_token: 'fixture-access', id_token } }, new Date(), []);
    expect(result.account).toBe(claims.email && claims.sub ? `${claims.email} (${claims.sub})` : claims.email ?? claims.sub);
  },
);

it('pairs a provider display name with the stable workspace identifier', () => {
  const result = tokenPair({ status: 200, headers: new Headers(), body: {
    token_type: 'Bearer', access_token: 'fixture-access', workspace_name: 'Ex\u202eam\u0000ple',
    workspace_id: 'workspace-42', owner: { user: { name: 'Mimicked account' } },
  } }, new Date(), []);
  expect(result.account).toBe('Example (workspace-42)');
});

it('displays a stronger account identity alongside a workspace name without an ID', () => {
  const result = tokenPair({ status: 200, headers: new Headers(), body: {
    token_type: 'Bearer', access_token: 'fixture-access', workspace_name: 'Example', email: 'user@example.net',
  } }, new Date(), []);
  expect(result.account).toBe('Example (user@example.net)');
});

it('quotes display names containing account delimiters before appending the provider ID', () => {
  const result = tokenPair({ status: 200, headers: new Headers(), body: {
    token_type: 'Bearer', access_token: 'fixture-access',
    account_name: 'Example (other-id) · provider.example', account_id: 'actual-id',
  } }, new Date(), []);
  expect(result.account).toBe('"Example (other-id) · provider.example" (actual-id)');
});
