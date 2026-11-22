// Sealing a secret to the vault (design §7.4), for the web app and the CLI.
// HPKE (RFC 9180) base mode, DHKEM(X25519, HKDF-SHA256), HKDF-SHA256,
// AES-256-GCM, over WebCrypto (works in browsers and Node 22+). The seal AAD
// binds org, credential, field, expected version, binding hash, ingest key
// ID and an expiry 10 minutes out, so the blob can only be stored as that
// field of that credential at that version with that destination, once.
//
// The blob is opaque to the core and runner-api: they relay it unchanged.
import { Aes256Gcm, CipherSuite, HkdfSha256 } from '@hpke/core';
import { DhkemX25519HkdfSha256 } from '@hpke/dhkem-x25519';
import type { IngestKeyResponse } from './contracts/internal.ts';
import { fromBase64Url, lengthPrefixed, toBase64Url } from './encoding.ts';

/** Blobs expire this long after sealing. */
export const SEAL_TTL_SECONDS = 600;
/** HPKE `info`: names the protocol and its version. */
export const SEAL_INFO = new TextEncoder().encode('runbook-runner/seal/v1');
/** The largest secret value accepted (bytes of UTF-8). */
export const MAX_SECRET_BYTES = 64 * 1024;

export type IngestKey = IngestKeyResponse;

export function sealSuite(): CipherSuite {
  return new CipherSuite({ kem: new DhkemX25519HkdfSha256(), kdf: new HkdfSha256(), aead: new Aes256Gcm() });
}

/** The fields the seal AAD binds; carried in clear in the blob. */
export interface SealBinding {
  org_id: string;
  credential_id: string;
  field: string;
  expected_version: number;
  /** base64url SHA-256 of the canonical destination binding. */
  binding_hash: string;
  ingest_key_id: string;
  /** Unix seconds. */
  expires_at: number;
}

export interface SealedBlob extends SealBinding {
  v: 1;
  /** HPKE encapsulated key, base64url. */
  enc: string;
  /** Ciphertext, base64url. */
  ct: string;
}

export function sealAad(b: SealBinding): Uint8Array {
  return lengthPrefixed([
    'runbook-runner/seal/v1',
    b.org_id,
    b.credential_id,
    b.field,
    String(b.expected_version),
    b.binding_hash,
    b.ingest_key_id,
    String(b.expires_at),
  ]);
}

export interface SealInput {
  orgId: string;
  credentialId: string;
  field: string;
  /** The secret's current version (0 when none); the vault refuses any other. */
  expectedVersion: number;
  /** The credential's binding hash as runner-api reports it (base64url, or raw bytes). */
  bindingHash: string | Uint8Array;
  value: string;
  /** Milliseconds since the epoch (Date.now()). */
  now: number;
}

/** Seal one secret field to the vault's ingest key. Returns a base64url blob. */
export async function sealSecret(publicKey: IngestKey, input: SealInput): Promise<string> {
  if (Date.parse(publicKey.expires_at) <= input.now) throw new Error('the ingest key has expired; fetch it again');
  const plaintext = new TextEncoder().encode(input.value);
  if (plaintext.length === 0) throw new Error('the secret is empty');
  if (plaintext.length > MAX_SECRET_BYTES) throw new Error('the secret is larger than 64 KiB');
  if (!Number.isSafeInteger(input.expectedVersion) || input.expectedVersion < 0) {
    throw new Error('expected version must be a non-negative integer');
  }
  const binding: SealBinding = {
    org_id: input.orgId,
    credential_id: input.credentialId,
    field: input.field,
    expected_version: input.expectedVersion,
    binding_hash: typeof input.bindingHash === 'string' ? input.bindingHash : toBase64Url(input.bindingHash),
    ingest_key_id: publicKey.ingest_key_id,
    expires_at: Math.floor(input.now / 1000) + SEAL_TTL_SECONDS,
  };
  const suite = sealSuite();
  const recipientPublicKey = await suite.kem.deserializePublicKey(fromBase64Url(publicKey.public_key));
  const sender = await suite.createSenderContext({ recipientPublicKey, info: SEAL_INFO });
  const ct = await sender.seal(plaintext, sealAad(binding));
  plaintext.fill(0);
  const blob: SealedBlob = { v: 1, ...binding, enc: toBase64Url(new Uint8Array(sender.enc)), ct: toBase64Url(new Uint8Array(ct)) };
  return toBase64Url(new TextEncoder().encode(JSON.stringify(blob)));
}

/** Parse a blob's envelope (no decryption). Throws on any malformed field. */
export function parseSealedBlob(blob: string): SealedBlob {
  if (typeof blob !== 'string' || blob.length > 4 * MAX_SECRET_BYTES) throw new Error('malformed blob');
  const parsed = JSON.parse(new TextDecoder().decode(fromBase64Url(blob))) as Partial<SealedBlob>;
  const str = (v: unknown, max = 256) => typeof v === 'string' && v.length > 0 && v.length <= max;
  const ok =
    parsed.v === 1 &&
    str(parsed.org_id) &&
    str(parsed.credential_id) &&
    str(parsed.field, 64) &&
    Number.isSafeInteger(parsed.expected_version) &&
    (parsed.expected_version ?? -1) >= 0 &&
    str(parsed.binding_hash, 64) &&
    str(parsed.ingest_key_id) &&
    Number.isSafeInteger(parsed.expires_at) &&
    str(parsed.enc, 128) &&
    str(parsed.ct, 2 * MAX_SECRET_BYTES);
  if (!ok) throw new Error('malformed blob');
  return parsed as SealedBlob;
}

/** SHA-256 of the raw public key, hex: what the vault logs at start and the CLI pins. */
export async function ingestKeyFingerprint(publicKey: IngestKey): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', fromBase64Url(publicKey.public_key));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('');
}
