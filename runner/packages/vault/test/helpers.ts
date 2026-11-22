// Shared setup for the vault's database tests: a throwaway database, a local
// KEK in a temp dir, a vault on an ephemeral port, and rows written the way
// runner-api would write them (with the admin pool, standing in for rb_api).
import { createHash, randomUUID } from 'node:crypto';
import { mkdtempSync } from 'node:fs';
import type { AddressInfo } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { Server } from 'node:http';
import { bindingHash } from '@runbook/runner-shared/binding';
import { captureLogs } from '@runbook/runner-shared/log';
import { createTestDatabase, type TestDatabase } from '@runbook/runner-shared/test-utils/pg';
import { JWT_TYP_USER, type UserAssertionClaims } from '@runbook/runner-shared/contracts/core';
import { generateSigningKey, signJwt } from '@runbook/runner-shared/test-utils/jwt';
import { parseSealedBlob } from '@runbook/runner-shared/seal-client';
import { generateLocalKeyFile, LocalKek } from '../src/kek/local.ts';
import { createVaultServer } from '../src/server.ts';
import { credentialBinding } from '../src/types/index.ts';
import { Vault, type VaultOptions } from '../src/vault.ts';

export const ORG = 'org_test';
export const GROUP = 'rg_default';
export const RUNNER = 'rn_one';
export const TOKENS = {
  api: 'fake-token-api-to-vault-000000000000000',
  worker: 'fake-token-worker-to-vault-0000000000000',
  workerProd: 'fake-token-worker-to-vault-prod-000000000',
  gateway: 'fake-token-vault-to-gateway-000000000000',
  release: 'fake-token-core-release-00000000000000000',
};

// Response bodies in tests are read loosely.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type Json = any;

export interface Harness {
  db: TestDatabase;
  vault: Vault;
  kek: LocalKek;
  url: string;
  clock: { now: number };
  logs: ReturnType<typeof captureLogs>;
  /** POST/GET to the vault with a caller's token. */
  call(method: string, path: string, body?: unknown, token?: string, assertion?: Partial<UserAssertionClaims> | false): Promise<{ status: number; text: string; json: Json }>;
  signAssertion(target: string, payload: string, overrides?: Partial<UserAssertionClaims>): string;
  close(): Promise<void>;
}

export async function startHarness(
  options: Partial<VaultOptions> & { clock?: { now: number } } = {},
): Promise<Harness> {
  const db = await createTestDatabase();
  const keyFile = join(mkdtempSync(join(tmpdir(), 'rb-vault-')), 'kek.json');
  await generateLocalKeyFile(keyFile);
  const kek = await LocalKek.open(keyFile);
  const { clock: givenClock, ...vaultOptions } = options;
  const clock = givenClock ?? { now: Date.now() };
  const logs = captureLogs();
  await db.admin().query(`INSERT INTO groups (group_id, name) VALUES ($1, 'default'), ('rg_prod_write', 'prod-write')`, [
    GROUP,
  ]);
  await db.admin().query(`INSERT INTO runners (runner_id, group_id, name) VALUES ($1, $2, 'one'), ('rn_prod', 'rg_prod_write', 'prod')`, [
    RUNNER,
    GROUP,
  ]);
  const signingKey = generateSigningKey('user-vault-fixture');
  const coreUrl = 'https://core.example';
  const installId = 'install_test';
  const vault = await Vault.create({
    pool: db.pool('vault'),
    kek,
    orgId: ORG,
    now: () => new Date(clock.now),
    renewIntervalMs: 0,
    assertion: { coreUrl, installId, fetch: async () => Response.json({ keys: [signingKey.jwk] }) },
    ...vaultOptions,
  });
  const server: Server = createVaultServer(vault, { inboundTokens: { api: TOKENS.api },
    workerTokens: { [RUNNER]: TOKENS.worker, rn_prod: TOKENS.workerProd } });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const url = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  const signAssertion: Harness['signAssertion'] = (target, payload, overrides) => {
    const iat = Math.floor(clock.now / 1000);
    const claims: UserAssertionClaims = {
      iss: coreUrl, aud: `runner:${installId}`, iat, exp: iat + 60, jti: randomUUID(),
      user_id: 'us_admin', org_id: ORG, principal: 'human', org_role: 'admin',
      project_roles: {}, scopes: ['runner:admin'], htm: 'POST', htu: target,
      bh: createHash('sha256').update(payload).digest('base64url'),
      ...overrides,
    };
    return signJwt(signingKey, JWT_TYP_USER, claims);
  };
  return {
    db,
    vault,
    kek,
    url,
    clock,
    logs,
    signAssertion,
    async call(method, path, body, token = TOKENS.api, assertion) {
      let target: string | undefined;
      let assertedMethod = method;
      if (path === '/internal/seal') {
        let credential_id = 'malformed';
        let field = 'malformed';
        try {
          ({ credential_id, field } = parseSealedBlob((body as { blob: string }).blob));
        } catch {
          // Exercise malformed ciphertext after authorization.
        }
        path = `${path}/${credential_id}/${field}`;
        target = `/runner/v1/credentials/${credential_id}/secrets/${field}`;
      } else if (path === '/internal/revoke') {
        const { credential_id, destroy } = body as { credential_id: string; destroy: boolean };
        path = `/internal/credentials/${credential_id}/${destroy ? 'destroy' : 'revoke'}`;
        target = `/runner/v1/credentials/${credential_id}${destroy ? '' : '/revoke'}`;
        assertedMethod = destroy ? 'DELETE' : 'POST';
        body = undefined;
      } else if (path === '/internal/credential-metadata') {
        target = '/runner/v1/credentials';
      } else if (path.startsWith('/internal/credential-metadata/')) {
        const id = path.split('/')[3]!;
        target = `/runner/v1/credentials/${id}`;
        assertedMethod = 'PATCH';
      } else if (path === '/internal/test') {
        const { credential_id } = body as { credential_id: string };
        path = `${path}/${credential_id}`;
        target = `/runner/v1/credentials/${credential_id}/test`;
        body = undefined;
      }
      const payload = body === undefined ? null : JSON.stringify(body);
      const headers: Record<string, string> = { authorization: `Bearer ${token}`, 'content-type': 'application/json' };
      if (target && assertion !== false) {
        headers['runbook-assertion'] = signAssertion(target, payload ?? '', { htm: assertedMethod, ...assertion });
      }
      const res = await fetch(url + path, { method, headers, body: payload });
      const text = await res.text();
      let json: unknown;
      try {
        json = JSON.parse(text);
      } catch {
        json = undefined;
      }
      return { status: res.status, text, json };
    },
    async close() {
      vault.stop();
      await new Promise<void>((resolve) => server.close(() => resolve()));
      logs.restore();
      await db.drop();
    },
  };
}

export interface CredentialSeed {
  id?: string;
  name?: string;
  type?: string;
  config?: Record<string, unknown>;
  groups?: 'all' | string[];
  sharing?: { projects: 'none' | 'all' | string[] };
  label?: string;
}

export const DATADOG_CONFIG = {
  hosts: ['api.datadoghq.com'],
  header_name: 'DD-API-KEY',
  header_format: '{}',
  test: { method: 'GET', path: '/api/v1/validate' },
};

/** Insert a credential as runner-api would (config, binding, binding hash). */
export async function insertCredential(h: Harness, seed: CredentialSeed = {}): Promise<{ id: string; hash: Buffer }> {
  const id = seed.id ?? `cr_${Math.random().toString(36).slice(2, 10)}`;
  const type = seed.type ?? 'api-key';
  const config = seed.config ?? DATADOG_CONFIG;
  const groups = seed.groups ?? 'all';
  const binding = credentialBinding(type, config, groups);
  const hash = bindingHash(type, binding);
  await h.db.admin().query(
    `INSERT INTO credentials (id, org_id, name, type, config, binding, binding_hash, groups, sharing, label, label_initialized, created_by)
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, true, 'us_admin')`,
    [
      id,
      ORG,
      seed.name ?? id,
      type,
      JSON.stringify(config),
      JSON.stringify(binding),
      hash,
      JSON.stringify(groups),
      JSON.stringify(seed.sharing ?? { projects: 'all' }),
      seed.label ?? 'read',
    ],
  );
  return { id, hash };
}

/** Change a credential's config the way runner-api (or a compromised one) could. */
export async function updateCredentialConfig(
  h: Harness,
  id: string,
  config: Record<string, unknown>,
  groups?: 'all' | string[],
): Promise<void> {
  await h.db.admin().query(`UPDATE credentials SET config = $2, groups = COALESCE($3, groups) WHERE id = $1`, [
    id,
    JSON.stringify(config),
    groups === undefined ? null : JSON.stringify(groups),
  ]);
}
