import { expect, it, vi } from 'vitest';
import { vaultRoutes } from '../src/server.ts';
import type { Vault } from '../src/vault.ts';

it('rejects process-only requests to seal or test credentials', async () => {
  const test = vi.fn(async () => ({ ok: true }));
  const vault = { releases: { test } } as unknown as Vault;
  const routes = vaultRoutes(vault);
  const seal = routes.find(({ path }) => path.startsWith('/internal/seal'))!;
  const testConnection = routes.find(({ path }) => path.startsWith('/internal/test'))!;
  const context = { caller: 'api' as const, params: { credential_id: 'cred', field: 'key' },
    request: { method: 'POST', headers: {} } };
  await expect(seal.handle({ ...context, body: { blob: 'invalid' } })).rejects.toMatchObject({ status: 401 });
  await expect(testConnection.handle({ ...context, body: { credential_id: 'cred' } })).rejects.toMatchObject({ status: 401 });
  expect(test).not.toHaveBeenCalled();
});
