// Destination bindings (design §7.3): where a credential's secret may go.
// The binding is canonicalized (RFC 8785, JSON Canonicalization Scheme) and
// hashed into every secret version's AAD, so changing where a secret may go
// makes it undecryptable until the secret is entered again. runner-api
// computes it to store and show; the vault recomputes it from the
// credential's current config at every seal and every unwrap.
import { createHash } from 'node:crypto';
import canonicalize from 'canonicalize';

export function canonicalJson(value: unknown): string {
  const text = canonicalize(value);
  if (text === undefined) throw new TypeError('value has no canonical JSON form');
  return text;
}

/** SHA-256 of the canonical JSON of `{type, binding}`. */
export function bindingHash(type: string, binding: object): Buffer {
  return createHash('sha256').update(canonicalJson({ type, binding }), 'utf8').digest();
}
