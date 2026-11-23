import { afterEach, expect, test, vi } from 'vitest';
import { runnerError, runnerRequest, runnerVaultIdentity } from './api';

afterEach(() => vi.unstubAllGlobals());

test('the runner proxy keeps its full path and query and sends CSRF on reads and writes', async () => {
  const fetch = vi.fn(async (request: Request) => {
    expect(request.headers.get('X-Runbook-CSRF')).toBe('1');
    return Response.json({ items: [] });
  });
  vi.stubGlobal('fetch', fetch);
  await expect(runnerRequest('ri_1', '/credentials?presets=true')).resolves.toEqual({ items: [] });
  await runnerRequest('ri_1', '/credentials/cred_1', 'PATCH', { name: 'Updated' });
  expect(fetch.mock.calls[0]?.[0].url).toContain(
    '/v1/runners/ri_1/proxy/runner/v1/credentials?presets=true',
  );
  const patch = fetch.mock.calls[1]?.[0];
  expect(patch?.url).toContain('/v1/runners/ri_1/proxy/runner/v1/credentials/cred_1');
  expect(patch?.method).toBe('PATCH');
  expect(await patch?.json()).toEqual({ name: 'Updated' });
});

test('a missing vault identity tells the operator who can enroll it', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () =>
      Response.json({ install_id: 'ri_1', status: 'online', runners: [], groups: [] }),
    ),
  );
  await expect(runnerVaultIdentity('org_1', 'ri_1').catch(runnerError)).resolves.toContain(
    'Ask an organization owner to register the vault identity',
  );
});

test('a revoked credential conflict tells the operator to re-create it', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () =>
      Response.json(
        { error: { code: 'credential.revoked', message: 'The credential is revoked.' } },
        { status: 409 },
      ),
    ),
  );
  await expect(
    runnerRequest('ri_1', '/credentials/cred_1/test', 'POST').catch(runnerError),
  ).resolves.toBe('The credential is revoked. Re-create it to use it again.');
});

test('reports a server fault with the request ID and leaves validation errors specific', async () => {
  vi.stubGlobal(
    'fetch',
    vi
      .fn()
      .mockResolvedValueOnce(
        Response.json(
          { error: { code: 'internal', message: 'internal error' } },
          { status: 500, headers: { 'X-Request-ID': 'request-fixture' } },
        ),
      )
      .mockResolvedValueOnce(
        Response.json(
          {
            error: {
              code: 'environment.invalid',
              message: 'setup_timeout_s must be between 1 and 600',
            },
          },
          { status: 422 },
        ),
      ),
  );
  await expect(
    runnerRequest('ri_1', '/environments/env_1', 'PATCH', {}).catch(runnerError),
  ).resolves.toBe('The runner could not complete this operation. Request ID: request-fixture.');
  await expect(
    runnerRequest('ri_1', '/environments/env_1', 'PATCH', {}).catch(runnerError),
  ).resolves.toBe('setup_timeout_s must be between 1 and 600');
});
