import { credentialBindingHash, type DestinationConfig } from './destination-binding';
import { fromBase64Url, lengthPrefixed } from './encoding';
import { sealSecret, type IngestKey } from './seal-client';

export interface CredentialBinding extends DestinationConfig {
  id: string;
  secret_meta: Record<string, { version: number; fingerprint?: string; created_at?: string }>;
}

export async function verifyVaultSignature(identityKey: string, signature: string, fields: string[]) {
  const publicKey = await crypto.subtle.importKey(
    'raw',
    fromBase64Url(identityKey),
    'Ed25519',
    false,
    ['verify'],
  );
  if (
    !(await crypto.subtle.verify(
      'Ed25519',
      publicKey,
      fromBase64Url(signature),
      new Uint8Array(lengthPrefixed(fields)),
    ))
  )
    throw new Error();
}

export async function verifyCredentialReceipt(
  reply: { id: string; creation_receipt?: string },
  expected: DestinationConfig & {
    name: string;
    label: string;
    sharing: { projects: string | string[] };
  },
  orgId: string,
  installId: string,
  identityKey: string,
) {
  try {
    if (!reply.creation_receipt || typeof reply.id !== 'string' || !reply.id) throw new Error();
    await verifyVaultSignature(identityKey, reply.creation_receipt, [
      'runbook-runner/credential-create/v1',
      orgId,
      installId,
      reply.id,
      expected.name,
      await credentialBindingHash(expected),
      expected.label,
      JSON.stringify(expected.sharing.projects),
    ]);
  } catch {
    throw new Error('Could not verify the vault credential receipt');
  }
}

async function verifyIngestKey(
  key: IngestKey,
  orgId: string,
  installId: string,
  identityKey?: string | null,
) {
  try {
    if (
      !identityKey ||
      !key.identity_signature ||
      key.org_id !== orgId ||
      key.install_id !== installId ||
      !Number.isFinite(Date.parse(key.expires_at)) ||
      Date.parse(key.expires_at) <= Date.now()
    )
      throw new Error();
    await verifyVaultSignature(identityKey, key.identity_signature, [
      'runbook-runner/ingest-key/v1',
      orgId,
      installId,
      key.ingest_key_id,
      key.public_key,
      key.expires_at,
    ]);
  } catch {
    throw new Error('Could not verify the vault identity for secret entry');
  }
}

// Read the ingest key only when submitting and forward only the sealed payload.
export async function saveCredentialSecrets(
  request: (path: string, method?: 'GET' | 'POST', body?: unknown) => Promise<unknown>,
  orgId: string,
  credential: CredentialBinding,
  values: Record<string, string>,
  anchor: { installId: string; identityKey?: string | null },
) {
  if (!Object.keys(values).length) return;
  const bindingHash = await credentialBindingHash(credential);
  const key = (await request('/ingest-key')) as IngestKey;
  await verifyIngestKey(key, orgId, anchor.installId, anchor.identityKey);
  for (const [field, value] of Object.entries(values)) {
    const blob = await sealSecret(key, {
      orgId,
      credentialId: credential.id,
      field,
      value,
      bindingHash,
      expectedVersion: credential.secret_meta[field]?.version ?? 0,
      now: Date.now(),
    });
    await request(`/credentials/${credential.id}/secrets/${field}`, 'POST', { blob });
  }
}
