// The runner's verifier for core JWTs: EdDSA only, typ, aud, issuer and
// 30 seconds of clock skew (plan Global Constraints, Review Focus 2).
import { describe, expect, it, vi } from 'vitest';
import { JwksCache, verifyJwt } from '../src/jwt.ts';
import { generateSigningKey, signJwt } from '../test-utils/jwt.ts';

const key = generateSigningKey('grant-k1');
const now = 1_800_000_000;
const claims = { iss: 'https://core.example', aud: 'runner:rn_1', sub: 'sr_1', iat: now, exp: now + 600 };
const options = { typ: 'rb-grant+jwt', aud: 'runner:rn_1', iss: 'https://core.example', now };

function cache(keys = [key.jwk]) {
  let fetches = 0;
  const jwks = new JwksCache(
    async () => {
      fetches++;
      return { keys };
    },
    { now: () => now * 1000 },
  );
  return { jwks, fetches: () => fetches };
}

describe('verifyJwt', () => {
  it('verifies a good token', async () => {
    const token = signJwt(key, 'rb-grant+jwt', claims);
    await expect(verifyJwt(cache().jwks, token, options)).resolves.toMatchObject({ sub: 'sr_1' });
  });

  it('accepts only EdDSA', async () => {
    const token = signJwt(key, 'rb-grant+jwt', claims, 'HS256');
    await expect(verifyJwt(cache().jwks, token, options)).rejects.toMatchObject({ code: 'jwt.algorithm' });
    const none = `${Buffer.from(JSON.stringify({ alg: 'none', typ: 'rb-grant+jwt', kid: 'grant-k1' })).toString('base64url')}.${Buffer.from(
      JSON.stringify(claims),
    ).toString('base64url')}.`;
    await expect(verifyJwt(cache().jwks, none, options)).rejects.toMatchObject({ code: 'jwt.algorithm' });
  });

  it('checks typ, aud, issuer and signature', async () => {
    const jwks = cache().jwks;
    await expect(verifyJwt(jwks, signJwt(key, 'rb-user+jwt', claims), options)).rejects.toMatchObject({ code: 'jwt.type' });
    await expect(verifyJwt(jwks, signJwt(key, 'rb-grant+jwt', { ...claims, aud: 'runner:rn_2' }), options)).rejects.toMatchObject({
      code: 'jwt.audience',
    });
    await expect(verifyJwt(jwks, signJwt(key, 'rb-grant+jwt', { ...claims, iss: 'https://evil.example' }), options)).rejects.toMatchObject({
      code: 'jwt.issuer',
    });
    const other = generateSigningKey('grant-k1');
    await expect(verifyJwt(jwks, signJwt(other, 'rb-grant+jwt', claims), options)).rejects.toMatchObject({ code: 'jwt.signature' });
    const tampered = signJwt(key, 'rb-grant+jwt', claims).split('.');
    tampered[1] = Buffer.from(JSON.stringify({ ...claims, sub: 'sr_2' })).toString('base64url');
    await expect(verifyJwt(jwks, tampered.join('.'), options)).rejects.toMatchObject({ code: 'jwt.signature' });
  });

  it('allows 30 seconds of skew and no more', async () => {
    const jwks = cache().jwks;
    const at = (exp: number, iat = exp - 600) => signJwt(key, 'rb-grant+jwt', { ...claims, exp, iat });
    await expect(verifyJwt(jwks, at(now - 29), options)).resolves.toBeDefined();
    await expect(verifyJwt(jwks, at(now - 31), options)).rejects.toMatchObject({ code: 'jwt.expired' });
    await expect(verifyJwt(jwks, at(now + 600, now + 29), options)).resolves.toBeDefined();
    await expect(verifyJwt(jwks, at(now + 600, now + 31), options)).rejects.toMatchObject({ code: 'jwt.not_yet_valid' });
  });

  it('refetches the JWKS for an unknown kid, at most every 30 s', async () => {
    const rotated = generateSigningKey('grant-k2');
    let keys = [key.jwk];
    let fetches = 0;
    let clock = now * 1000;
    const jwks = new JwksCache(
      async () => {
        fetches++;
        return { keys };
      },
      { now: () => clock },
    );
    await verifyJwt(jwks, signJwt(key, 'rb-grant+jwt', claims), options);
    keys = [key.jwk, rotated.jwk];
    // Too soon after the last fetch: the new key isn't known yet.
    await expect(verifyJwt(jwks, signJwt(rotated, 'rb-grant+jwt', claims), options)).rejects.toMatchObject({ code: 'jwt.unknown_key' });
    clock += 31_000;
    await expect(verifyJwt(jwks, signJwt(rotated, 'rb-grant+jwt', claims), options)).resolves.toBeDefined();
    expect(fetches).toBe(2);
  });

  it('drops a key removed from the JWKS once the cache is older than the max age', async () => {
    let keys = [key.jwk];
    let fetches = 0;
    let clock = now * 1000;
    const jwks = new JwksCache(
      async () => {
        fetches++;
        return { keys };
      },
      { now: () => clock },
    );
    const token = signJwt(key, 'rb-grant+jwt', claims);
    await verifyJwt(jwks, token, options);
    keys = [];
    clock += 299_000;
    await expect(verifyJwt(jwks, token, options)).resolves.toBeDefined();
    expect(fetches).toBe(1);
    // Past the default max age of 5 minutes the whole map is replaced.
    clock += 2_000;
    await expect(verifyJwt(jwks, token, options)).rejects.toMatchObject({ code: 'jwt.unknown_key' });
    expect(fetches).toBe(2);
  });

  it('takes a configurable max age, and fails closed for unknown kids when the refresh fails', async () => {
    const rotated = generateSigningKey('grant-k2');
    let fail = false;
    let clock = now * 1000;
    const jwks = new JwksCache(
      async () => {
        if (fail) throw new Error('jwks unreachable');
        return { keys: [key.jwk] };
      },
      { now: () => clock, maxAgeMs: 60_000 },
    );
    await verifyJwt(jwks, signJwt(key, 'rb-grant+jwt', claims), options);
    fail = true;
    clock += 61_000;
    // A known key keeps verifying while the JWKS can't be fetched …
    await expect(verifyJwt(jwks, signJwt(key, 'rb-grant+jwt', claims), options)).resolves.toBeDefined();
    // … an unknown kid never does.
    await expect(verifyJwt(jwks, signJwt(rotated, 'rb-grant+jwt', claims), options)).rejects.toThrow();
    clock += 31_000;
    await expect(verifyJwt(jwks, signJwt(rotated, 'rb-grant+jwt', claims), options)).rejects.toThrow();
  });

  it('never trusts a known key past the hard deadline during a JWKS outage', async () => {
    let clock = now * 1000;
    const load = vi.fn().mockResolvedValueOnce({ keys: [key.jwk] }).mockRejectedValue(new Error('offline'));
    const jwks = new JwksCache(load, { now: () => clock });
    const token = signJwt(key, 'rb-grant+jwt', claims);
    await expect(verifyJwt(jwks, token, options)).resolves.toBeDefined();
    clock += 3_599_000;
    await expect(verifyJwt(jwks, token, options)).resolves.toBeDefined();
    clock += 1_000;
    await expect(verifyJwt(jwks, token, options)).rejects.toMatchObject({ code: 'jwt.jwks_unavailable' });
    expect(load).toHaveBeenCalledTimes(2);
  });

  it('honors the configured hard deadline independently of refresh cadence', async () => {
    vi.stubEnv('RUNNER_JWKS_MAX_AGE_SECONDS', '600');
    try {
      let clock = now * 1000;
      const load = vi.fn().mockResolvedValueOnce({ keys: [key.jwk] }).mockRejectedValue(new Error('offline'));
      const jwks = new JwksCache(load, { now: () => clock });
      await expect(jwks.key(key.jwk.kid)).resolves.toBeDefined();
      clock += 599_000;
      await expect(jwks.key(key.jwk.kid)).resolves.toBeDefined();
      clock += 1_000;
      await expect(jwks.key(key.jwk.kid)).rejects.toMatchObject({ code: 'jwt.jwks_unavailable' });
    } finally {
      vi.unstubAllEnvs();
    }
  });

  it('ties the kid to the token type: a grant signed by a user- key is refused', async () => {
    const userKey = generateSigningKey('user-abc');
    const grantKey = generateSigningKey('grant-abc');
    const jwks = cache([userKey.jwk, grantKey.jwk]).jwks;
    await expect(verifyJwt(jwks, signJwt(userKey, 'rb-grant+jwt', claims), options)).rejects.toMatchObject({
      code: 'jwt.unknown_key',
    });
    await expect(verifyJwt(jwks, signJwt(grantKey, 'rb-grant+jwt', claims), options)).resolves.toBeDefined();
    const userOptions = { ...options, typ: 'rb-user+jwt' };
    await expect(verifyJwt(jwks, signJwt(grantKey, 'rb-user+jwt', claims), userOptions)).rejects.toMatchObject({
      code: 'jwt.unknown_key',
    });
    await expect(verifyJwt(jwks, signJwt(userKey, 'rb-user+jwt', claims), userOptions)).resolves.toBeDefined();
    // An explicit prefix in the options wins.
    await expect(verifyJwt(jwks, signJwt(grantKey, 'rb-grant+jwt', claims), { ...options, kidPrefix: 'grant-x' })).rejects.toMatchObject({
      code: 'jwt.unknown_key',
    });
  });
});
