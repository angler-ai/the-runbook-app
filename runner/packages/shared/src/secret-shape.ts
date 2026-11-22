// Secret-shaped values in credential config (design §7.2): config is stored
// in clear and shown in the UI, so a value that looks like a secret is
// refused with "put this in a secret field". Findings name the path and the
// reason, never the value.

export type SecretShapeReason = 'known-prefix' | 'private-key' | 'url-credentials' | 'high-entropy';

export interface SecretShapeFinding {
  path: string;
  reason: SecretShapeReason;
}

/** Token prefixes of common providers (and the core's own tokens). */
const KNOWN_PREFIX =
  /(?:^|[^A-Za-z0-9])(?:sk-ant-[A-Za-z0-9_-]{8,}|sk-(?:proj-)?[A-Za-z0-9_-]{20,}|gh[pousr]_[A-Za-z0-9]{16,}|github_pat_[A-Za-z0-9_]{16,}|xox[a-z]-[A-Za-z0-9-]{8,}|xapp-[A-Za-z0-9-]{8,}|(?:AKIA|ASIA)[A-Z0-9]{16}|AIza[A-Za-z0-9_-]{30,}|glpat-[A-Za-z0-9_-]{16,}|hf_[A-Za-z0-9]{20,}|npm_[A-Za-z0-9]{20,}|SG\.[A-Za-z0-9_-]{16,}|trb_(?:pat|svc|exe|exr)_[A-Za-z0-9_-]{16,})/;
const PRIVATE_KEY = /-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----/;
const URL_CREDENTIALS = /[A-Za-z][A-Za-z0-9+.-]*:\/\/[^/\s:@]*:[^/\s@]+@/;
const TOKEN_LIKE = /[A-Za-z0-9+/=_-]{32,}/g;

function entropy(text: string): number {
  const counts = new Map<string, number>();
  for (const ch of text) counts.set(ch, (counts.get(ch) ?? 0) + 1);
  let bits = 0;
  for (const n of counts.values()) {
    const p = n / text.length;
    bits -= p * Math.log2(p);
  }
  return bits;
}

/** A run of 32+ token characters with mixed letters and digits and high entropy (hex IDs and UUIDs pass). */
function highEntropy(text: string): boolean {
  for (const m of text.matchAll(TOKEN_LIKE)) {
    const run = m[0];
    if (/^[0-9a-f-]+$/i.test(run)) continue; // hex digests, UUIDs, commit SHAs
    const classes = [/[a-z]/, /[A-Z]/, /[0-9]/].filter((r) => r.test(run)).length;
    if (classes >= 2 && entropy(run) >= 4.2) return true;
  }
  return false;
}

function classify(value: string): SecretShapeReason | undefined {
  if (PRIVATE_KEY.test(value)) return 'private-key';
  if (KNOWN_PREFIX.test(value)) return 'known-prefix';
  if (URL_CREDENTIALS.test(value)) return 'url-credentials';
  if (highEntropy(value)) return 'high-entropy';
  return undefined;
}

export function findSecretShapedValues(value: unknown, path = ''): SecretShapeFinding[] {
  if (typeof value === 'string') {
    const reason = classify(value);
    return reason ? [{ path: path || '(value)', reason }] : [];
  }
  if (Array.isArray(value)) return value.flatMap((v, i) => findSecretShapedValues(v, `${path}[${i}]`));
  if (typeof value === 'object' && value !== null) {
    return Object.entries(value).flatMap(([k, v]) => findSecretShapedValues(v, path ? `${path}.${k}` : k));
  }
  return [];
}
