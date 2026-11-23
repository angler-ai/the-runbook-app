// The vault's core client (design §7.5): asks for grants with its
// release-only token (`runner:release`) and verifies every grant it gets or
// is shown against the core's JWKS: EdDSA only, typ rb-grant+jwt, aud
// runner:<runner_id>, the core as issuer, 30 s of skew.
import {
  type GrantClaims,
  type GrantRequest,
  type GrantResponse,
  JWT_SKEW_SECONDS,
  JWT_TYP_GRANT,
  LEASE_SECONDS,
} from '@runbook/runner-shared/contracts/core';
import { hasGrantPolicy } from '@runbook/runner-shared/capability';
import { CoreError, coreJson } from '@runbook/runner-shared/core-client';
import { JwksCache, JwtError, verifyJwt } from '@runbook/runner-shared/jwt';
import { VaultError } from './errors.ts';

/** The longest a grant may still have to run: one lease plus clock skew. */
export const MAX_GRANT_SECONDS = LEASE_SECONDS + JWT_SKEW_SECONDS;

export interface CoreOptions {
  baseUrl: string;
  releaseToken: string;
  /** Expected `iss` of grants (default: baseUrl). */
  issuer?: string;
  fetch?: typeof fetch;
  /** Refetch the core's JWKS once it is this old (default 5 minutes). */
  jwksMaxAgeMs?: number;
}

/** What a grant must say, checked field by field. */
export type GrantExpectation = Partial<
  Pick<
    GrantClaims,
    | 'sub'
    | 'kind'
    | 'claim_id'
    | 'generation'
    | 'runner_id'
    | 'group_id'
    | 'agent_id'
    | 'agent_version'
    | 'project_id'
    | 'credential_id'
    | 'credential_version'
    | 'delivery'
  >
>;

export class CoreGrants {
  private readonly o: CoreOptions;
  private readonly jwks: JwksCache;
  private readonly now: () => Date;

  constructor(options: CoreOptions, now: () => Date) {
    this.o = options;
    this.now = now;
    this.jwks = JwksCache.fromUrl(`${options.baseUrl}/.well-known/jwks.json`, options.fetch, {
      ...(options.jwksMaxAgeMs !== undefined ? { maxAgeMs: options.jwksMaxAgeMs } : {}),
    });
  }

  /** Verify a grant JWT and that it says what is expected. Throws JwtError or VaultError. */
  async verify(token: string, expect: GrantExpectation): Promise<GrantClaims> {
    const now = Math.floor(this.now().getTime() / 1000);
    const claims = await verifyJwt<GrantClaims>(this.jwks, token, {
      typ: JWT_TYP_GRANT,
      aud: (aud) => aud.startsWith('runner:'),
      iss: this.o.issuer ?? this.o.baseUrl,
      now,
    });
    if (!hasGrantPolicy(claims)) throw new JwtError('jwt.claims', 'missing or invalid grant policy');
    // A grant lives no longer than a lease (renewals extend it).
    if (claims.exp - now > MAX_GRANT_SECONDS) throw new JwtError('jwt.lifetime', 'grant outlives a lease');
    if (claims.aud !== `runner:${claims.runner_id}`) throw new JwtError('jwt.audience', 'token is for another audience');
    for (const [k, v] of Object.entries(expect)) {
      if (v !== undefined && (claims as unknown as Record<string, unknown>)[k] !== v) {
        throw new JwtError('jwt.claims', 'grant does not match the request');
      }
    }
    if (claims.kind === 'capability' && (claims.credential_id ?? claims.credential_version ?? claims.delivery) !== undefined) {
      throw new JwtError('jwt.claims', 'a capability names no credential');
    }
    return claims;
  }

  /** Ask the core for a grant and verify it. Core refusals become VaultErrors with the core's code. */
  async grant(stepRunId: string, request: GrantRequest, expect: GrantExpectation): Promise<{ token: string; claims: GrantClaims }> {
    let response: GrantResponse;
    try {
      response = await coreJson<GrantResponse>(
        { baseUrl: this.o.baseUrl, token: this.o.releaseToken, ...(this.o.fetch ? { fetch: this.o.fetch } : {}) },
        'POST',
        `/v1/step-runs/${encodeURIComponent(stepRunId)}/grants`,
        request,
      );
    } catch (e) {
      if (e instanceof CoreError && (e.status === 409 || e.status === 403)) throw new VaultError(e.status, e.code, 'the core refused the grant');
      throw new VaultError(503, 'core.unavailable', 'the core server could not be reached');
    }
    try {
      const claims = await this.verify(response.grant, { ...expect, sub: stepRunId, kind: request.kind });
      return { token: response.grant, claims };
    } catch (e) {
      if (e instanceof JwtError) throw new VaultError(502, 'release.invalid_grant', 'the core returned a grant that does not verify');
      throw e;
    }
  }

  /** Record a revocation in the core (`credential.revoked`). Best effort. */
  async revoked(stepRunId: string, credentialId: string, reason: string): Promise<void> {
    await coreJson(
      { baseUrl: this.o.baseUrl, token: this.o.releaseToken, ...(this.o.fetch ? { fetch: this.o.fetch } : {}) },
      'POST',
      `/v1/step-runs/${encodeURIComponent(stepRunId)}/grants/revoked`,
      { credential_id: credentialId, reason },
    );
  }
}
