import { createHash } from 'node:crypto';
import { JWT_SKEW_SECONDS, JWT_TYP_USER, USER_ASSERTION_TTL_SECONDS, type UserAssertionClaims } from './contracts/core.ts';
import { HttpError } from './http.ts';
import { JwksCache, verifyJwt } from './jwt.ts';

export interface AssertionOptions {
  consumeReplay(install: string, jti: string, expiresAt: Date): Promise<boolean>;
  coreUrl: string;
  installId: string;
  orgId: string;
  now?: () => number;
  fetch?: typeof fetch;
}

export class AssertionVerifier {
  private readonly options: AssertionOptions;
  private readonly jwks: JwksCache;

  constructor(options: AssertionOptions) {
    this.options = options;
    this.jwks = JwksCache.fromUrl(`${options.coreUrl}/.well-known/jwks.json`, options.fetch);
  }

  async verifyAssertion(input: { token: string; method: string; target: string; body: Buffer }): Promise<UserAssertionClaims> {
    const timestamp = this.options.now ?? Date.now;
    let claims: UserAssertionClaims;
    try {
      claims = await verifyJwt<UserAssertionClaims>(this.jwks, input.token, {
        typ: JWT_TYP_USER, iss: this.options.coreUrl, aud: `runner:${this.options.installId}`,
        now: Math.floor(timestamp() / 1000),
      });
      if (!Number.isSafeInteger(claims.iat) || !Number.isSafeInteger(claims.exp) ||
          claims.exp <= claims.iat || claims.exp - claims.iat > USER_ASSERTION_TTL_SECONDS ||
          typeof claims.jti !== 'string' || !claims.jti || claims.jti.length > 128 ||
          typeof claims.user_id !== 'string' || !claims.user_id ||
          claims.org_id !== this.options.orgId || !['human', 'agent'].includes(claims.principal) ||
          !['owner', 'admin', 'member'].includes(claims.org_role) ||
          !claims.project_roles || typeof claims.project_roles !== 'object' || Array.isArray(claims.project_roles) ||
          Object.values(claims.project_roles).some((role) => !['viewer', 'operator', 'editor', 'admin'].includes(role)) ||
          !Array.isArray(claims.scopes) ||
          claims.htm !== input.method || claims.htu !== input.target ||
          claims.bh !== createHash('sha256').update(input.body).digest('base64url')) throw new Error('invalid assertion');
    } catch {
      throw new HttpError(401, 'assertion.invalid', 'invalid runner assertion');
    }
    const now = Math.floor(timestamp() / 1000);
    const expires = claims.exp + JWT_SKEW_SECONDS;
    if (expires <= now) throw new HttpError(401, 'assertion.invalid', 'invalid runner assertion');
    if (!await this.options.consumeReplay(this.options.installId, claims.jti, new Date(expires * 1000))) {
      throw new HttpError(401, 'assertion.replayed', 'runner assertion already used');
    }
    return claims;
  }
}
