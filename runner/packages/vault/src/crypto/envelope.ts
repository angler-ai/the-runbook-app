// Envelope encryption of secret versions (design §7.3): a fresh 256-bit data
// key and a random 96-bit nonce per version, AES-256-GCM over the plaintext
// with the length-prefixed AAD (aad.ts), and the data key wrapped by the KEK
// under the wrap context {org_id, credential_id}. Data keys are zeroed as
// soon as they're used.
import { createCipheriv, createDecipheriv, randomBytes } from 'node:crypto';
import type { Kek, WrapContext } from '../kek/index.ts';
import { type AadFields, encodeAad } from './aad.ts';

const TAG_BYTES = 16;
const NONCE_BYTES = 12;
const KEY_BYTES = 32;

export interface SealedRow {
  /** Ciphertext followed by the 16-byte GCM tag. */
  ciphertext: Buffer;
  nonce: Buffer;
  wrappedDek: Buffer;
  kekRef: string;
  bindingHash: Buffer;
}

/** Errors carry no key, ciphertext or plaintext material. */
export class VaultCryptoError extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(message);
    this.name = new.target.name;
    this.code = code;
  }
}
/** The credential's current destination binding differs from the one the secret was sealed for. */
export class BindingMismatch extends VaultCryptoError {
  constructor() {
    super('vault.binding_mismatch', 'the credential destination changed; enter the secret again');
  }
}
/** Authentication failed: wrong key, context, AAD or tampered data. */
export class DecryptError extends VaultCryptoError {
  constructor(what = 'secret') {
    super('vault.decrypt_failed', `could not decrypt the ${what}`);
  }
}

export function wrapContextOf(aad: Pick<AadFields, 'orgId' | 'credentialId'>): WrapContext {
  return { org_id: aad.orgId, credential_id: aad.credentialId };
}

export function aesGcmEncrypt(key: Buffer, plaintext: Buffer, aad: Buffer): { nonce: Buffer; ciphertext: Buffer } {
  const nonce = randomBytes(NONCE_BYTES);
  const cipher = createCipheriv('aes-256-gcm', key, nonce, { authTagLength: TAG_BYTES });
  cipher.setAAD(aad);
  const ciphertext = Buffer.concat([cipher.update(plaintext), cipher.final(), cipher.getAuthTag()]);
  return { nonce, ciphertext };
}

export function aesGcmDecrypt(key: Buffer, nonce: Buffer, ciphertext: Buffer, aad: Buffer, what?: string): Buffer {
  if (nonce.length !== NONCE_BYTES || ciphertext.length < TAG_BYTES) throw new DecryptError(what);
  try {
    const decipher = createDecipheriv('aes-256-gcm', key, nonce, { authTagLength: TAG_BYTES });
    decipher.setAAD(aad);
    decipher.setAuthTag(ciphertext.subarray(ciphertext.length - TAG_BYTES));
    return Buffer.concat([decipher.update(ciphertext.subarray(0, ciphertext.length - TAG_BYTES)), decipher.final()]);
  } catch {
    throw new DecryptError(what);
  }
}

export async function seal(kek: Kek, plaintext: Buffer, aad: AadFields, wrapContext: WrapContext): Promise<SealedRow> {
  const dek = randomBytes(KEY_BYTES);
  try {
    const { nonce, ciphertext } = aesGcmEncrypt(dek, plaintext, encodeAad(aad));
    const { wrapped, kekRef } = await kek.wrap(dek, wrapContext);
    return { ciphertext, nonce, wrappedDek: wrapped, kekRef, bindingHash: Buffer.from(aad.bindingHash) };
  } finally {
    dek.fill(0);
  }
}

/**
 * Decrypt a row. `aad.bindingHash` must be recomputed by the caller from the
 * credential's current config, never read from the row.
 */
export async function open(kek: Kek, row: SealedRow, aad: AadFields): Promise<Buffer> {
  if (!row.bindingHash.equals(aad.bindingHash)) throw new BindingMismatch();
  const dek = await kek.unwrap(row.wrappedDek, row.kekRef, wrapContextOf(aad));
  try {
    return aesGcmDecrypt(dek, row.nonce, row.ciphertext, encodeAad(aad));
  } finally {
    dek.fill(0);
  }
}

/** Re-wrap a row's data key under the KEK's current version; the ciphertext is unchanged. */
export async function rewrap(kek: Kek, row: SealedRow, wrapContext: WrapContext): Promise<SealedRow> {
  const { wrapped, kekRef } = await rewrapKey(kek, row.wrappedDek, row.kekRef, wrapContext);
  return { ...row, wrappedDek: wrapped, kekRef };
}

export async function rewrapKey(kek: Kek, wrapped: Buffer, kekRef: string, wrapContext: WrapContext) {
  const key = await kek.unwrap(wrapped, kekRef, wrapContext);
  try {
    return await kek.wrap(key, wrapContext);
  } finally {
    key.fill(0);
  }
}
