// Verifying the core's JWTs (grants `rb-grant+jwt`, user assertions
// `rb-user+jwt`): EdDSA (Ed25519) only, keys from the core's JWKS by `kid`,
// 30 seconds of clock skew. Verification fails closed with a fixed code.
import { createPublicKey, verify as verifySignature, type KeyObject } from 'node:crypto';
import { JWT_ALG, JWT_KID_PREFIX, JWT_SKEW_SECONDS, type Jwk, type Jwks } from './contracts/core.ts';

import { JWKS_MAX_AGE_ERROR, validJwksMaxAge } from './config.ts';

export class JwtError extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(message);
    this.name = 'JwtError';
    this.code = code;
  }
}

export interface JwtHeader {
  alg: string;
  typ: string;
  kid: string;
}

function decodePart(part: string | undefined): unknown {
  if (part === undefined || !/^[A-Za-z0-9_-]+$/.test(part)) throw new JwtError('jwt.malformed', 'malformed token');
  try {
    return JSON.parse(Buffer.from(part, 'base64url').toString('utf8'));
  } catch {
    throw new JwtError('jwt.malformed', 'malformed token');
  }
}

/** Read the header without verifying anything (to pick the key). */
export function peekHeader(token: string): JwtHeader {
  if (typeof token !== 'string' || token.length > 8192) throw new JwtError('jwt.malformed', 'malformed token');
  const header = decodePart(token.split('.')[0]) as Partial<JwtHeader>;
  if (typeof header !== 'object' || header === null) throw new JwtError('jwt.malformed', 'malformed token');
  return header as JwtHeader;
}

/**
 * Fetches and caches a JWKS. Refetches for an unknown `kid` at most every
 * 30 s, and replaces the whole map once it is older than the max age
 * (default 5 minutes) so keys the core removed stop verifying. A failed
 * refresh keeps the last map for known keys and fails closed for unknown ones.
 */
export class JwksCache {
  private keys = new Map<string, KeyObject>();
  private fetchedAt = 0;
  private attemptedAt = 0;
  private inflight: Promise<void> | undefined;
  private readonly load: () => Promise<Jwks>;
  private readonly now: () => number;
  private readonly minRefreshMs: number;
  private readonly maxAgeMs: number;
  private readonly hardMaxAgeMs: number;

  constructor(load: () => Promise<Jwks>, options: JwksCacheOptions = {}) {
    this.load = load;
    this.now = options.now ?? Date.now;
    this.minRefreshMs = options.minRefreshMs ?? 30_000;
    const hardAgeSeconds = Number(process.env.RUNNER_JWKS_MAX_AGE_SECONDS ?? 3600);
    if (!validJwksMaxAge(hardAgeSeconds)) {
      throw new Error(JWKS_MAX_AGE_ERROR);
    }
    this.hardMaxAgeMs = hardAgeSeconds * 1000;
    this.maxAgeMs = Math.min(options.maxAgeMs ?? JWKS_MAX_AGE_MS, this.hardMaxAgeMs);
  }

  static fromUrl(url: string, fetchImpl: typeof fetch = fetch, options: JwksCacheOptions = {}): JwksCache {
    return new JwksCache(async () => {
      const res = await fetchImpl(url, { redirect: 'error', signal: AbortSignal.timeout(10_000) });
      if (!res.ok) throw new JwtError('jwt.jwks_unavailable', 'could not fetch the JWKS');
      return (await res.json()) as Jwks;
    }, options);
  }

  private async refresh(): Promise<void> {
    this.inflight ??= (async () => {
      this.attemptedAt = this.now();
      try {
        const jwks = await this.load();
        const next = new Map<string, KeyObject>();
        for (const k of jwks.keys ?? []) {
          const jwk = k as Partial<Jwk>;
          if (jwk.kty !== 'OKP' || jwk.crv !== 'Ed25519' || typeof jwk.x !== 'string' || typeof jwk.kid !== 'string') continue;
          if (jwk.alg !== undefined && jwk.alg !== JWT_ALG) continue;
          if (jwk.use !== undefined && jwk.use !== 'sig') continue;
          next.set(jwk.kid, createPublicKey({ key: { kty: 'OKP', crv: 'Ed25519', x: jwk.x }, format: 'jwk' }));
        }
        this.keys = next;
        this.fetchedAt = this.now();
      } finally {
        this.inflight = undefined;
      }
    })();
    return this.inflight;
  }

  async key(kid: string): Promise<KeyObject | undefined> {
    const now = this.now();
    const due = now - this.attemptedAt >= this.minRefreshMs;
    if (this.fetchedAt === 0) {
      await this.refresh();
    } else if (now - this.fetchedAt >= this.maxAgeMs && due) {
      try {
        await this.refresh();
      } catch (e) {
        if (!this.keys.has(kid)) throw e;
      }
    } else if (!this.keys.has(kid) && due) {
      await this.refresh();
    }
    if (this.now() - this.fetchedAt >= this.hardMaxAgeMs) {
      throw new JwtError('jwt.jwks_unavailable', 'cached JWKS is too old');
    }
    return this.keys.get(kid);
  }
}

/** How long a fetched JWKS is trusted before the whole map is refetched. */
export const JWKS_MAX_AGE_MS = 5 * 60_000;

export interface JwksCacheOptions {
  now?: () => number;
  /** Least time between fetches (default 30 s). */
  minRefreshMs?: number;
  /** Refetch (replacing every key) once the map is this old (default 5 minutes). */
  maxAgeMs?: number;
}

export interface VerifyOptions {
  typ: string;
  /** Required `kid` prefix (default: the core's prefix for `typ`; an unknown `typ` without one is refused). */
  kidPrefix?: string;
  /** Expected `aud`, or a predicate. */
  aud: string | ((aud: string) => boolean);
  iss?: string;
  /** Unix seconds. */
  now: number;
}

/** Verify signature, algorithm, type, audience, issuer and time. Returns the claims. */
export async function verifyJwt<C extends { exp: number; iat?: number; aud: string; iss: string }>(
  jwks: JwksCache,
  token: string,
  options: VerifyOptions,
): Promise<C> {
  const header = peekHeader(token);
  if (header.alg !== JWT_ALG) throw new JwtError('jwt.algorithm', 'only EdDSA tokens are accepted');
  if (header.typ !== options.typ) throw new JwtError('jwt.type', 'wrong token type');
  if (typeof header.kid !== 'string' || header.kid === '') throw new JwtError('jwt.malformed', 'token has no key ID');
  // A kid names its purpose: a token of one type never verifies with another purpose's key.
  const kidPrefix = options.kidPrefix ?? (Object.hasOwn(JWT_KID_PREFIX, options.typ) ? JWT_KID_PREFIX[options.typ] : undefined);
  if (kidPrefix === undefined || !header.kid.startsWith(kidPrefix)) {
    throw new JwtError('jwt.unknown_key', 'token signed by an unknown key');
  }
  const parts = token.split('.');
  if (parts.length !== 3) throw new JwtError('jwt.malformed', 'malformed token');
  const key = await jwks.key(header.kid);
  if (!key) throw new JwtError('jwt.unknown_key', 'token signed by an unknown key');
  const signature = Buffer.from(parts[2] ?? '', 'base64url');
  const ok = verifySignature(null, Buffer.from(`${parts[0]}.${parts[1]}`), key, signature);
  if (!ok) throw new JwtError('jwt.signature', 'bad token signature');
  const claims = decodePart(parts[1]) as C;
  if (typeof claims !== 'object' || claims === null) throw new JwtError('jwt.malformed', 'malformed token');
  const audOk = typeof options.aud === 'string' ? claims.aud === options.aud : typeof claims.aud === 'string' && options.aud(claims.aud);
  if (!audOk) throw new JwtError('jwt.audience', 'token is for another audience');
  if (options.iss !== undefined && claims.iss !== options.iss) throw new JwtError('jwt.issuer', 'token from another issuer');
  if (!Number.isFinite(claims.exp)) throw new JwtError('jwt.malformed', 'token has no expiry');
  if (claims.exp <= options.now - JWT_SKEW_SECONDS) throw new JwtError('jwt.expired', 'token expired');
  if (claims.iat !== undefined && claims.iat > options.now + JWT_SKEW_SECONDS) {
    throw new JwtError('jwt.not_yet_valid', 'token issued in the future');
  }
  return claims;
}
