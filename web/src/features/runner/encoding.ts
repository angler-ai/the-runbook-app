// Byte encodings shared by the vault and the sealing client (browser and
// Node, so Uint8Array and WebCrypto-free helpers only).

const utf8 = new TextEncoder();

/**
 * Length-prefixed concatenation: each part is preceded by its byte length as
 * a 4-byte big-endian integer, so no two different lists encode the same
 * (`["ab","c"]` ≠ `["a","bc"]`). Used for every AAD (design §7.3, §7.4).
 */
export function lengthPrefixed(parts: readonly (string | Uint8Array)[]): Uint8Array {
  const encoded = parts.map((p) => (typeof p === 'string' ? utf8.encode(p) : p));
  const total = encoded.reduce((n, p) => n + 4 + p.length, 0);
  const out = new Uint8Array(total);
  const view = new DataView(out.buffer);
  let at = 0;
  for (const p of encoded) {
    if (p.length > 0xffffffff) throw new RangeError('AAD part too long');
    view.setUint32(at, p.length, false);
    out.set(p, at + 4);
    at += 4 + p.length;
  }
  return out;
}

export function toBase64Url(bytes: Uint8Array): string {
  let binary = '';
  for (const b of bytes) binary += String.fromCharCode(b);
  return btoa(binary).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '');
}

export function fromBase64Url(text: string): Uint8Array<ArrayBuffer> {
  if (!/^[A-Za-z0-9_-]*$/.test(text)) throw new Error('not base64url');
  const padded =
    text.replaceAll('-', '+').replaceAll('_', '/') + '==='.slice((text.length + 3) % 4);
  const binary = atob(padded);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}
