import { createPublicKey, verify } from 'node:crypto';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
// HPKE ingest keys and sealed secret writes (design §7.4), through the
// vault's HTTP server with runner-api's process token.
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { verifyAuditChain } from '@runbook/runner-shared/audit';
import { bindingHash } from '@runbook/runner-shared/binding';
import type { IngestKeyResponse } from '@runbook/runner-shared/contracts/internal';
import { hasTestDatabase } from '@runbook/runner-shared/test-utils/pg';
import { sealSecret } from '@runbook/runner-shared/seal-client';
import { credentialBinding } from '../src/types/index.ts';
import { DATADOG_CONFIG, type Harness, insertCredential, ORG, startHarness, TOKENS, updateCredentialConfig } from './helpers.ts';

const SECRET_V1 = 'fake-dd-key-v1-AAAAAAAAAAAAAAAAAAAA';
const SECRET_V2 = 'fake-dd-key-v2-BBBBBBBBBBBBBBBBBBBB';
const HOUR = 3600_000;

describe.skipIf(!hasTestDatabase)('vault seal', () => {
  let h: Harness;
  let credential: { id: string; hash: Buffer };
  let key: IngestKeyResponse;

  beforeEach(async () => {
    h = await startHarness();
    credential = await insertCredential(h);
    const res = await h.call('GET', '/internal/ingest-key');
    expect(res.status).toBe(200);
    key = res.json;
  });
  afterEach(async () => h?.close());

  const seal = (value: string, over: Partial<Parameters<typeof sealSecret>[1]> = {}) =>
    sealSecret(key, {
      orgId: ORG,
      credentialId: credential.id,
      field: 'key',
      expectedVersion: 0,
      bindingHash: credential.hash,
      value,
      now: h.clock.now,
      ...over,
    });

  function assertNoSecretLeak(text: string) {
    for (const s of [SECRET_V1, SECRET_V2]) {
      expect(text).not.toContain(s);
      expect(text).not.toContain(Buffer.from(s).toString('base64'));
    }
  }

  it('refuses to seal a sandbox placeholder as a credential', async () => {
    const blob = await seal('rb-placeholder-fixture');
    const response = await h.call('POST', '/internal/seal', { blob });
    expect(response.status).toBe(400);
    expect(response.json.error.code).toBe('seal.placeholder');
    expect(await h.vault.secrets.latestVersion(credential.id, 'key')).toBeUndefined();
  });

  it('attests ingest keys with a persistent, KEK-sealed identity', async () => {
    const { VaultIdentity } = await import('../src/identity.ts');
    const { lengthPrefixed } = await import('@runbook/runner-shared/encoding');
    const persisted = await VaultIdentity.load(h.vault.pool, h.kek);
    expect(persisted.publicKey).toBe(h.vault.identity.publicKey);
    const jwk = createPublicKey({ format: 'jwk', key: { kty: 'OKP', crv: 'Ed25519', x: persisted.publicKey } });
    const signature = Buffer.from(key.identity_signature!, 'base64url');
    const message = (publicKey: string) => lengthPrefixed([
      'runbook-runner/ingest-key/v1', ORG, 'install_test', key.ingest_key_id, publicKey, key.expires_at,
    ]);
    expect(verify(null, message(key.public_key), jwk, signature)).toBe(true);
    expect(verify(null, message('substituted'), jwk, signature)).toBe(false);
    const rows = await h.db.admin().query("SELECT wrapped FROM vault_keys WHERE purpose = 'ed25519-identity'");
    expect(rows.rowCount).toBe(1);
    expect(rows.rows[0].wrapped).toHaveLength(60);
  });

  it('signs the credential creation reply with the enrolled vault identity', async () => {
    const { lengthPrefixed } = await import('@runbook/runner-shared/encoding');
    const result = await h.call('POST', '/internal/credential-metadata', {
      name: 'receipt credential', type: 'mcp-bearer', config: { url: 'https://mcp.example' },
      label: 'read', sharing: { projects: ['p'] },
    });
    expect(result.status).toBe(200);
    const { id, name, binding_hash, label, sharing, creation_receipt } = result.json;
    const identity = createPublicKey({ format: 'jwk', key: {
      kty: 'OKP', crv: 'Ed25519', x: h.vault.identity.publicKey,
    } });
    const statement = lengthPrefixed([
      'runbook-runner/credential-create/v1', ORG, 'install_test', id, name,
      binding_hash, label, JSON.stringify(sharing.projects),
    ]);
    expect(verify(null, statement, identity, Buffer.from(creation_receipt, 'base64url'))).toBe(true);
  });

  it('loads the public identity enrolled by Compose and keeps an owner rotation across restarts', async () => {
    const { VaultIdentity } = await import('../src/identity.ts');
    const { LocalKek } = await import('../src/kek/local.ts');
    const { composeIdentitySeed, vaultIdentityPrivateKey, vaultIdentityPublicKey } = await import('@runbook/runner-shared/vault-identity');
    const directory = await mkdtemp(join(tmpdir(), 'rb-compose-identity-'));
    try {
      const key = Buffer.alloc(32, 11);
      const file = join(directory, 'kek.json');
      await writeFile(file, JSON.stringify({ format: 'runbook-runner-local-kek/1', key_id: '0123456789abcdef',
        current: 1, keys: { 1: key.toString('base64') }, bootstrap_identity: true }), { mode: 0o600 });
      const expected = vaultIdentityPublicKey(vaultIdentityPrivateKey(composeIdentitySeed(key)));
      await h.db.admin().query("DELETE FROM vault_keys WHERE purpose = 'ed25519-identity'");
      const kek = await LocalKek.open(file);
      const bootstrapped = await VaultIdentity.load(h.vault.pool, kek);
      expect(bootstrapped.publicKey).toBe(expected);
      const rotated = await VaultIdentity.rotate(h.vault.pool, kek);
      expect(rotated.publicKey).not.toBe(expected);
      expect((await VaultIdentity.load(h.vault.pool, await LocalKek.open(file))).publicKey).toBe(rotated.publicKey);
      expect((await h.db.admin().query(`
        SELECT actor, action FROM audit_log WHERE resource_type = 'vault_identity' ORDER BY seq
      `)).rows).toEqual([
        { actor: { kind: 'process', id: 'installer bootstrap' }, action: 'vault.identity_enrolled' },
        { actor: { kind: 'process', id: 'vault-operator' }, action: 'vault.identity_rotated' },
      ]);
      await expect(verifyAuditChain(h.db.admin())).resolves.toEqual({ ok: true });
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });

  it('rewraps credential, ingest and identity keys after KEK rotation without changing ciphertext', async () => {
    const blob = await seal(SECRET_V1);
    expect((await h.call('POST', '/internal/seal', { blob })).status).toBe(200);
    const db = h.db.admin();
    const row = async () => (await db.query('SELECT ciphertext, nonce, kek_ref FROM credential_secret_versions')).rows[0];
    const before = await row();
    await h.kek.rotate();
    await h.vault.rewrapKeys();
    const after = await row();
    expect(after.ciphertext).toEqual(before.ciphertext);
    expect(after.nonce).toEqual(before.nonce);
    expect(after.kek_ref).not.toBe(before.kek_ref);
    const refs = await db.query('SELECT kek_ref FROM ingest_keys UNION SELECT kek_ref FROM vault_keys');
    expect(refs.rows).toEqual([{ kek_ref: after.kek_ref }]);
    expect((await h.vault.secrets.openForRelease({ credentialId: credential.id })).value.toString()).toBe(SECRET_V1);
    expect(await h.vault.rewrapKeys()).toBe(0);
  });

  it('seal_then_store_round_trip', async () => {
    const res = await h.call('POST', '/internal/seal', { blob: await seal(SECRET_V1) });
    expect(res.status).toBe(200);
    // Metadata only.
    expect(Object.keys(res.json).sort()).toEqual(['credential_id', 'field', 'fingerprint', 'version']);
    expect(res.json).toMatchObject({ credential_id: credential.id, field: 'key', version: 1 });
    expect(res.json.fingerprint).toMatch(/^[0-9a-f]{8}$/);
    assertNoSecretLeak(res.text);
    // Stored encrypted; only the vault can read it back.
    const { rows } = await h.db.admin().query(`SELECT * FROM credential_secret_versions WHERE credential_id = $1`, [
      credential.id,
    ]);
    expect(rows).toHaveLength(1);
    expect(Buffer.from(rows[0].ciphertext).includes(Buffer.from(SECRET_V1))).toBe(false);
    expect(rows[0].fingerprint).toBe(res.json.fingerprint);
    const plain = await h.vault.secrets.openForRelease({ credentialId: credential.id, field: 'key' });
    expect(plain.value.toString()).toBe(SECRET_V1);
    expect(plain.version).toBe(1);
    // Audited without the value.
    const audit = await h.db.admin().query(`SELECT action, detail::text FROM audit_log`);
    expect(audit.rows.map((r) => r.action)).toContain('credential.secret_set');
    assertNoSecretLeak(JSON.stringify(audit.rows));
    assertNoSecretLeak(h.logs.text());
  });

  it('requires vault-confirmed label policy before activating an uninitialized credential', async () => {
    await h.db.admin().query('UPDATE credentials SET label_initialized = false WHERE id = $1', [credential.id]);
    const blob = await seal(SECRET_V1);
    const denied = await h.call('POST', '/internal/seal', { blob });
    expect(denied.status).toBe(409);
    expect(denied.json.error.code).toBe('credential.policy_required');
    const label = await h.call('POST', `/internal/credential-metadata/${credential.id}`, { label: 'destructive' });
    expect(label.status).toBe(200);
    const retry = await h.call('POST', '/internal/seal', { blob });
    expect(retry.status).toBe(200);
    expect((await h.vault.secrets.credential(credential.id)).label).toBe('destructive');
  });

  it('requires the original credential field, body and fresh assertion', async () => {
    const body = { blob: await seal(SECRET_V1) };
    const call = (assertion: Parameters<Harness['call']>[4]) =>
      h.call('POST', '/internal/seal', body, TOKENS.api, assertion);
    expect((await call(false)).status).toBe(401);
    expect((await call({ aud: 'runner:another-install' })).status).toBe(401);
    expect((await call({ htm: 'PATCH' })).status).toBe(401);
    expect((await call({ htu: `/runner/v1/credentials/other/secrets/key` })).status).toBe(401);
    expect((await call({ bh: 'tampered' })).status).toBe(401);
    expect((await call({ org_role: 'member' })).status).toBe(403);
    expect((await call({ principal: 'agent' })).status).toBe(403);
    expect((await call({ jti: 'fresh-seal' })).status).toBe(200);
    expect((await call({ jti: 'fresh-seal' })).json.error.code).toBe('assertion.replayed');
  });

  it('rolls back a failed audit and exposes the committed version after a lost acknowledgement', async () => {
    const body = { blob: await seal(SECRET_V1) };
    const admin = h.db.admin();
    const blocker = await admin.connect();
    try {
      await blocker.query('BEGIN');
      await blocker.query('SELECT * FROM audit_chain_head FOR UPDATE');
      expect((await h.call('POST', '/internal/seal', body)).json.error.code).toBe('audit.unavailable');
    } finally {
      await blocker.query('ROLLBACK');
      blocker.release();
    }
    expect(await h.vault.secrets.currentVersion(credential.id, 'key')).toBe(0);
    // Ignore the response as a client whose connection closed after commit would.
    await h.call('POST', '/internal/seal', body);
    const { rows: [credentialMetadata] } = await h.db.pool('api').query(
      'SELECT secret_meta, status FROM credentials WHERE id = $1', [credential.id],
    );
    expect(credentialMetadata).toMatchObject({ status: 'active', secret_meta: { key: { version: 1 } } });
    const next = await seal(SECRET_V2, { expectedVersion: credentialMetadata.secret_meta.key.version });
    expect((await h.call('POST', '/internal/seal', { blob: next })).json.version).toBe(2);
  });

  it('expired_blob_refused', async () => {
    const blob = await seal(SECRET_V1, { now: h.clock.now - 11 * 60_000 });
    const res = await h.call('POST', '/internal/seal', { blob });
    expect(res.status).toBe(400);
    expect(res.json.error.code).toBe('seal.expired');
    // A blob claiming a far-future expiry is refused too (clock in the past).
    const future = await seal(SECRET_V1, { now: h.clock.now + 60 * 60_000 });
    expect((await h.call('POST', '/internal/seal', { blob: future })).json.error.code).toBe('seal.expiry_too_far');
  });

  it('replayed_blob_refused', async () => {
    const blob = await seal(SECRET_V1);
    expect((await h.call('POST', '/internal/seal', { blob })).status).toBe(200);
    const again = await h.call('POST', '/internal/seal', { blob });
    expect(again.status).toBe(409);
    expect(again.json.error.code).toBe('seal.replayed');
    // The replay cache keeps entries for 24 hours.
    const { rows } = await h.db.admin().query(`SELECT count(*)::int AS n FROM seal_replay`);
    expect(rows[0].n).toBe(1);
    h.clock.now += 25 * HOUR;
    await h.call('POST', '/internal/seal', { blob: await seal(SECRET_V2, { expectedVersion: 1 }) });
    const after = await h.db.admin().query(`SELECT count(*)::int AS n FROM seal_replay`);
    expect(after.rows[0].n).toBe(1);
  });

  it('stale_expected_version_refused', async () => {
    expect((await h.call('POST', '/internal/seal', { blob: await seal(SECRET_V1) })).status).toBe(200);
    // Rollback: a blob made for version 0 can't overwrite version 1.
    const stale = await h.call('POST', '/internal/seal', { blob: await seal(SECRET_V2, { expectedVersion: 0 }) });
    expect(stale.status).toBe(409);
    expect(stale.json.error.code).toBe('seal.stale_version');
    // Nor can one skip ahead.
    const ahead = await h.call('POST', '/internal/seal', { blob: await seal(SECRET_V2, { expectedVersion: 3 }) });
    expect(ahead.json.error.code).toBe('seal.stale_version');
    const ok = await h.call('POST', '/internal/seal', { blob: await seal(SECRET_V2, { expectedVersion: 1 }) });
    expect(ok.json.version).toBe(2);
  });

  it('blob_for_other_binding_refused', async () => {
    const otherHash = bindingHash('api-key', credentialBinding('api-key', { ...DATADOG_CONFIG, hosts: ['evil.example'] }, 'all'));
    const res = await h.call('POST', '/internal/seal', { blob: await seal(SECRET_V1, { bindingHash: otherHash }) });
    expect(res.status).toBe(409);
    expect(res.json.error.code).toBe('seal.binding_mismatch');
    // A blob sealed for the old destination, after the destination changed.
    const blob = await seal(SECRET_V1);
    await updateCredentialConfig(h, credential.id, { ...DATADOG_CONFIG, hosts: ['api.datadoghq.eu'] });
    expect((await h.call('POST', '/internal/seal', { blob })).json.error.code).toBe('seal.binding_mismatch');
  });

  it('a blob relabelled for another credential or field fails to open', async () => {
    const other = await insertCredential(h);
    const blob = await seal(SECRET_V1);
    const doc = JSON.parse(Buffer.from(blob, 'base64url').toString());
    for (const change of [{ credential_id: other.id, binding_hash: other.hash.toString('base64url') }, { field: 'other' }, { expected_version: 1 }]) {
      const forged = Buffer.from(JSON.stringify({ ...doc, ...change })).toString('base64url');
      const res = await h.call('POST', '/internal/seal', { blob: forged });
      expect(res.status).toBe(400);
      expect(res.json.error.code).toBe('seal.decrypt_failed');
    }
    expect((await h.call('POST', '/internal/seal', { blob: 'not-a-blob' })).json.error.code).toBe('seal.malformed');
  });

  it('only runner-api may seal or read the ingest key', async () => {
    const blob = await seal(SECRET_V1);
    expect((await h.call('POST', '/internal/seal', { blob }, TOKENS.worker)).status).toBe(403);
    expect((await h.call('POST', '/internal/seal', { blob }, 'fake-wrong-token-000000000000000000000000')).status).toBe(401);
    expect((await h.call('GET', '/internal/ingest-key', undefined, TOKENS.worker)).status).toBe(403);
  });

  it('ingest keys rotate monthly and old keys open until they expire', async () => {
    const blob = await seal(SECRET_V1);
    h.clock.now += 31 * 24 * HOUR - 5 * 60_000;
    const next = (await h.call('GET', '/internal/ingest-key')).json as IngestKeyResponse;
    expect(next.ingest_key_id).not.toBe(key.ingest_key_id);
    expect(Date.parse(next.expires_at)).toBeGreaterThan(h.clock.now + 30 * 24 * HOUR);
    // The blob made with the old key is past its own 10 minutes by now, so
    // check with a fresh blob for the old key just before that key expires.
    expect((await h.call('POST', '/internal/seal', { blob })).json.error.code).toBe('seal.expired');
    const late = await seal(SECRET_V1);
    expect((await h.call('POST', '/internal/seal', { blob: late })).status).toBe(200);
    h.clock.now += 10 * 60_000;
    const tooLate = await sealSecret({ ...key, expires_at: new Date(h.clock.now + 60_000).toISOString() }, {
      orgId: ORG,
      credentialId: credential.id,
      field: 'key',
      expectedVersion: 1,
      bindingHash: credential.hash,
      value: SECRET_V2,
      now: h.clock.now,
    });
    expect((await h.call('POST', '/internal/seal', { blob: tooLate })).json.error.code).toBe('seal.unknown_key');
  });

  it('rotation_keeps_pinned_version_for_live_grant', async () => {
    await h.call('POST', '/internal/seal', { blob: await seal(SECRET_V1) });
    h.clock.now += HOUR;
    const rotated = await h.call('POST', '/internal/seal', { blob: await seal(SECRET_V2, { expectedVersion: 1 }) });
    expect(rotated.json.version).toBe(2);
    const secrets = h.vault.secrets;
    // A release under an existing grant for version 1 still opens version 1…
    const pinned = await secrets.openForRelease({ credentialId: credential.id, field: 'key', pinnedVersion: 1 });
    expect(pinned.value.toString()).toBe(SECRET_V1);
    // …a new grant gets version 2…
    const fresh = await secrets.openForRelease({ credentialId: credential.id, field: 'key' });
    expect(fresh).toMatchObject({ version: 2 });
    expect(fresh.value.toString()).toBe(SECRET_V2);
    // …and version 1 is not handed to anything without a grant pinned to it.
    expect(await secrets.latestVersion(credential.id, 'key')).toBe(2);
    // Within 24 hours of the rotation it still opens; after, it doesn't, and
    // the sweep deletes its wrapped data key.
    h.clock.now += 23 * HOUR;
    expect((await secrets.openForRelease({ credentialId: credential.id, field: 'key', pinnedVersion: 1 })).version).toBe(1);
    expect(await secrets.destroyExpiredVersions()).toBe(0);
    h.clock.now += 2 * HOUR;
    await expect(
      secrets.openForRelease({ credentialId: credential.id, field: 'key', pinnedVersion: 1 }),
    ).rejects.toMatchObject({ code: 'secret.version_expired' });
    expect(await secrets.destroyExpiredVersions()).toBe(1);
    const { rows } = await h.db.admin().query(
      `SELECT version, wrapped_dek IS NULL AS gone, destroyed_at IS NOT NULL AS destroyed
       FROM credential_secret_versions WHERE credential_id = $1 ORDER BY version`,
      [credential.id],
    );
    expect(rows).toEqual([
      { version: 1, gone: true, destroyed: true },
      { version: 2, gone: false, destroyed: false },
    ]);
    await expect(
      secrets.openForRelease({ credentialId: credential.id, field: 'key', pinnedVersion: 1 }),
    ).rejects.toMatchObject({ code: 'secret.version_destroyed' });
    expect((await secrets.openForRelease({ credentialId: credential.id, field: 'key' })).value.toString()).toBe(SECRET_V2);
    assertNoSecretLeak(h.logs.text());
  });
});
