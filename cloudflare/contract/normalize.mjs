// CF9: the contract suite's normalizer. Two builds answer the same
// scripted requests; their responses are compared after this turns what
// legitimately differs between two runs into placeholders:
// - IDs (`prefix_` + 26-character ULID, and the derived `sr_` IDs);
// - secrets (`trb_…` tokens, `whsec_…` webhook secrets) and signatures;
// - times: integers in `*_ms` / `*_at` fields, ISO-8601 and RFC 3339
//   strings, `YYYY-MM-DD` dates in archive names, and 13-digit epoch
//   milliseconds inside strings;
// - each origin (the two builds listen on different ports);
// - ETags, digests of run-specific content and request IDs.
// Everything else (field names, types, status values, error codes, counts,
// order) must match. Arrays keep their order unless the contract leaves it
// undefined, listed in UNORDERED by key.

/** Keys whose arrays the contract leaves unordered (sorted before
 * comparing). IDs are ULIDs, so lists ordered by ID or time keep the same
 * order on both builds; only a list whose order the contract leaves
 * undefined belongs here. */
// Undelivered notices with the same timestamp tie-break by event ID,
// which is derived from each build's distinct execution ID.
export const UNORDERED = new Set(['notifications_not_delivered']);

// `\_`: the Markdown report escapes the underscore (LANG4: the parent
// a child names).
const ID = /\b[a-z]{2,5}(?:\\)?_[0-9A-HJKMNP-TV-Z]{26}\b/g;
const SECRET = /\b(?:trb_[a-z]+_[A-Za-z0-9_-]{20,}|whsec_[A-Za-z0-9_-]{20,})\b/g;
const ISO = /\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})\b/g;
const DATE = /\b(?:19|20|21)\d{2}-\d{2}-\d{2}\b/g;
const EPOCH_MS = /\b1\d{12}\b/g;
const HEX64 = /\b[0-9a-f]{64}\b/g;
const SIG = /([?&]sig=)[A-Za-z0-9_-]+/g;
const EXPIRES = /([?&]expires=)\d+/g;

function placeholderFor(id) {
  return `<${id.slice(0, id.indexOf('_')).replace(/\\$/, '')}>`;
}

/** Normalize one string. `origins` maps each build's origin to a name. */
export function normalizeText(text, origins = []) {
  let out = text;
  for (const [origin, name] of origins) out = out.split(origin).join(name);
  return out
    .replace(SECRET, '<secret>')
    .replace(ID, placeholderFor)
    .replace(SIG, '$1<sig>')
    .replace(EXPIRES, '$1<ms>')
    .replace(ISO, '<time>')
    .replace(DATE, '<date>')
    .replace(EPOCH_MS, '<ms>')
    .replace(HEX64, '<sha256>');
}

const TIME_KEY = /(_ms|_at|^at|^created|^updated|^expires|^date)$/;
// Content-derived digests that differ because they hash IDs or times
// (semantic hashes of sources naming per-build users, request hashes).
const RUN_DIGEST_KEY = new Set(['etag', 'request_hash', 'semantic_hash', 'snapshot_digest', 'compiled_hash', 'hash']);

/** Normalize a JSON value for comparison. */
export function normalize(value, origins = [], key = '') {
  if (value === null || value === undefined) return value ?? null;
  if (Array.isArray(value)) {
    const items = value.map((item) => normalize(item, origins, key));
    if (RUN_DIGEST_KEY.has(key) && items.every((n) => typeof n === 'number')) return '<digest>';
    if (UNORDERED.has(key)) {
      return items.sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b)));
    }
    return items;
  }
  if (typeof value === 'object') {
    const out = {};
    for (const k of Object.keys(value).sort()) out[k] = normalize(value[k], origins, k);
    return out;
  }
  if (typeof value === 'number') {
    if (TIME_KEY.test(key) || /^1\d{12}$/.test(String(value))) return '<ms>';
    return value;
  }
  if (typeof value === 'string') {
    if (RUN_DIGEST_KEY.has(key)) return '<digest>';
    return normalizeText(value, origins);
  }
  return value;
}
