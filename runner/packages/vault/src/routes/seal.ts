// POST /internal/seal (design §7.4, step 5): open an HPKE blob sealed in the
// browser or CLI, check it was made for this credential's current
// destination, at its current version, recently and only once, then store
// it under a fresh data key. Answers with metadata only.
import { createHash } from 'node:crypto';
import { canonicalJson } from '@runbook/runner-shared/binding';
import { isCredentialPlaceholder, secretManagerReference } from '@runbook/runner-shared/credential-source';
import { JWT_SKEW_SECONDS } from '@runbook/runner-shared/contracts/core';
import type { SealRequest, SealResponse } from '@runbook/runner-shared/contracts/internal';
import { withTransaction } from '@runbook/runner-shared/db';
import { fromBase64Url, toBase64Url } from '@runbook/runner-shared/encoding';
import { MAX_SECRET_BYTES, parseSealedBlob, SEAL_INFO, SEAL_TTL_SECONDS, sealAad, sealSuite } from '@runbook/runner-shared/seal-client';
import type { SealedBlob } from '@runbook/runner-shared/seal-client';
import { audit } from '../audit.ts';
import { VaultError } from '../errors.ts';
import { typeImpl } from '../types/index.ts';
import type { Vault } from '../vault.ts';

export const REPLAY_WINDOW_MS = 24 * 3600_000;

export async function handleSeal(
  vault: Vault, body: SealRequest, actor: { kind: 'user'; id: string }, expected?: { credentialId: string; field: string },
): Promise<SealResponse> {
  let blob: SealedBlob;
  try {
    blob = parseSealedBlob(body?.blob);
  } catch {
    throw new VaultError(400, 'seal.malformed', 'the sealed blob is malformed');
  }
  if (expected && (blob.credential_id !== expected.credentialId || blob.field !== expected.field)) {
    throw new VaultError(400, 'seal.wrong_target', 'the blob was sealed for another credential field');
  }
  const now = vault.now();
  const nowSeconds = Math.floor(now.getTime() / 1000);
  if (blob.expires_at <= nowSeconds - JWT_SKEW_SECONDS) {
    throw new VaultError(400, 'seal.expired', 'the sealed blob has expired; seal it again');
  }
  if (blob.expires_at > nowSeconds + SEAL_TTL_SECONDS + JWT_SKEW_SECONDS) {
    throw new VaultError(400, 'seal.expiry_too_far', 'the sealed blob expires too far in the future');
  }

  const privateKey = await vault.ingest.privateKey(blob.ingest_key_id);
  if (!privateKey) throw new VaultError(400, 'seal.unknown_key', 'the ingest key is unknown or expired; fetch it again');

  let plaintext: Buffer;
  let enc: Uint8Array;
  try {
    enc = fromBase64Url(blob.enc);
    const recipient = await sealSuite().createRecipientContext({ recipientKey: privateKey, enc, info: SEAL_INFO });
    plaintext = Buffer.from(await recipient.open(fromBase64Url(blob.ct), sealAad(blob)));
  } catch {
    throw new VaultError(400, 'seal.decrypt_failed', 'the sealed blob does not open with its stated binding');
  }

  try {
    if (plaintext.length === 0 || plaintext.length > MAX_SECRET_BYTES) {
      throw new VaultError(413, 'seal.too_large', 'the secret must be between 1 byte and 64 KiB');
    }
    if (blob.org_id !== vault.orgId) throw new VaultError(403, 'seal.wrong_org', 'the blob was sealed for another org');
    const digest = createHash('sha256').update(enc).digest();
    const stored = await withTransaction(vault.pool, async (client) => {
      await client.query('SELECT pg_advisory_xact_lock(hashtextextended($1, 7419006))', [blob.credential_id]);
      const credential = await vault.secrets.credential(blob.credential_id, client, true);
      if (credential.status === 'revoked') throw new VaultError(409, 'credential.revoked', 'the credential is revoked');
      if (!credential.label_initialized) {
        throw new VaultError(409, 'credential.policy_required', 'save the credential label before sealing');
      }
      const oauth = credential.type === 'mcp-oauth';
      const config = credential.config as { registration_mode?: string; token_endpoint_auth_method?: string; issuer?: string };
      if (oauth && (blob.field !== 'client_secret' || config?.registration_mode !== 'pre-registered' ||
          !config.issuer || config.token_endpoint_auth_method === 'none')) {
        throw new VaultError(400, 'credential.provider_managed', 'OAuth token fields are managed by the vault');
      }
      const fields = typeImpl(credential.type).secretFields;
      if (!fields.includes(blob.field)) {
        throw new VaultError(400, 'credential.not_secret_field', 'not a secret field of this credential type');
      }
      const { binding, hash } = vault.secrets.binding(credential);
      if (toBase64Url(hash) !== blob.binding_hash) {
        throw new VaultError(409, 'seal.binding_mismatch', 'the credential destination changed since this value was sealed');
      }
      const reference = binding.secret_source;
      if (reference) {
        if (plaintext.toString('utf8') !== canonicalJson(reference)) {
          throw new VaultError(400, 'seal.reference_mismatch', 'seal the canonical Secret Manager reference to confirm it');
        }
        vault.secrets.assertReferenceAllowed(secretManagerReference(reference), credential.type, binding);
      }
      if (isCredentialPlaceholder(plaintext.toString('utf8'))) {
        throw new VaultError(400, 'seal.placeholder', 'a sandbox placeholder cannot be used as a credential');
      }
      await client.query('DELETE FROM seal_replay WHERE seen_at < $1', [new Date(now.getTime() - REPLAY_WINDOW_MS)]);
      const fresh = await client.query(
        'INSERT INTO seal_replay (digest, seen_at) VALUES ($1, $2) ON CONFLICT (digest) DO NOTHING',
        [digest, now],
      );
      if (fresh.rowCount !== 1) throw new VaultError(409, 'seal.replayed', 'this sealed blob was already used');
      const version = await vault.secrets.write(client, {
        credential,
        field: blob.field,
        expectedVersion: blob.expected_version,
        value: plaintext,
        createdBy: `${actor.kind}:${actor.id}`,
      });
      await client.query(
        `UPDATE credentials SET secret_meta = jsonb_set(secret_meta, ARRAY[$2], $3),
           status = CASE WHEN $6 THEN status WHEN NOT EXISTS (
             SELECT 1 FROM unnest($4::text[]) AS required(field)
             WHERE required.field <> $2 AND secret_meta->required.field->'sealed_for_binding' IS DISTINCT FROM 'true'::jsonb
           ) THEN 'active' ELSE 'needs_secret' END, updated_at = $5 WHERE id = $1`,
        [credential.id, blob.field, JSON.stringify({
          version: version.version, fingerprint: version.fingerprint, sealed_for_binding: true,
          created_at: now.toISOString(), created_by: actor.id,
          ...(reference !== undefined && { source: 'gcp-secret-manager' }),
        }), fields, now, oauth],
      );
      await audit(client, {
        actor,
        action: 'credential.secret_set',
        resource_type: 'credential',
        resource_id: blob.credential_id,
        group_id: null,
        runner_id: null,
        detail: { field: blob.field, version: version.version, fingerprint: version.fingerprint,
          ingest_key_id: blob.ingest_key_id, ...(reference !== undefined && { source: 'gcp-secret-manager' }) },
      });
      return version;
    });
    vault.log.info('secret stored', { credential_id: blob.credential_id, field: blob.field, version: stored.version });
    return { credential_id: blob.credential_id, field: blob.field, version: stored.version, fingerprint: stored.fingerprint };
  } finally {
    plaintext.fill(0);
  }
}
