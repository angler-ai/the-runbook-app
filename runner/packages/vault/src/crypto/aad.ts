// The data AAD of a secret version (design §7.3): a length-prefixed encoding
// of org_id, credential_id, field, version, type, binding_hash. The KEK
// version is deliberately not in it (it lives in the wrap context), so
// re-wrapping a data key under a new KEK version never re-encrypts data.
import { lengthPrefixed as lengthPrefixedBytes } from '@runbook/runner-shared/encoding';

export interface AadFields {
  orgId: string;
  credentialId: string;
  field: string;
  version: number;
  type: string;
  bindingHash: Buffer;
}

export function lengthPrefixed(parts: readonly (string | Uint8Array)[]): Buffer {
  const bytes = lengthPrefixedBytes(parts);
  return Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength);
}

export function encodeAad(f: AadFields): Buffer {
  if (!Number.isSafeInteger(f.version) || f.version < 1) throw new RangeError('version must be a positive integer');
  if (f.bindingHash.length !== 32) throw new RangeError('binding hash must be 32 bytes');
  return lengthPrefixed([f.orgId, f.credentialId, f.field, String(f.version), f.type, f.bindingHash]);
}
