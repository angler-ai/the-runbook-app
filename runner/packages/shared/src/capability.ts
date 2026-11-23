import { createHash } from 'node:crypto';
import { type GrantClaims, JWT_SKEW_SECONDS, JWT_TYP_GRANT, LEASE_SECONDS } from './contracts/core.ts';
import { HttpError } from './http.ts';
import { JwksCache, verifyJwt } from './jwt.ts';

interface Options {
  coreUrl: string;
  fetch?: typeof fetch;
  now?: () => number;
  runnerIds: readonly string[];
}

/** Runtime validation of the trusted policy required by runner contract 2. */
export function hasGrantPolicy(grant: GrantClaims): boolean {
  const ids = (value: unknown): boolean => Array.isArray(value) &&
    value.every((entry: unknown) => typeof entry === 'string' && entry.length > 0);
  const limit = (value: unknown) => value === null ||
    (typeof value === 'number' && Number.isSafeInteger(value) && value > 0);
  return typeof grant.destructive === 'boolean' &&
    (grant.reach === undefined || (grant.reach === 'read' && !grant.destructive && ids(grant.network) && ids(grant.connections))) &&
    (grant.network === null || ids(grant.network)) &&
    (grant.connections === null || ids(grant.connections)) &&
    (grant.model_profile === null || (typeof grant.model_profile === 'string' && grant.model_profile.length > 0)) &&
    limit(grant.max_tokens) && limit(grant.timeout_ms) && limit(grant.step_deadline_ms);
}

export class CapabilityVerifier {
  private readonly jwks: JwksCache;

  private readonly options: Options;

  constructor(options: Options) {
    this.options = options;
    this.jwks = JwksCache.fromUrl(`${options.coreUrl}/.well-known/jwks.json`, options.fetch);
  }

  async verifyCapability(token: string): Promise<GrantClaims> {
    const now = Math.floor((this.options.now ?? Date.now)() / 1000);
    try {
      const grant = await verifyJwt<GrantClaims>(this.jwks, token, {
        typ: JWT_TYP_GRANT, iss: this.options.coreUrl, aud: (aud) => aud.startsWith('runner:'), now,
      });
      if (grant.kind === 'capability' && hasGrantPolicy(grant) && grant.exp > now && grant.exp - now <= LEASE_SECONDS + JWT_SKEW_SECONDS &&
          grant.aud === `runner:${grant.runner_id}` &&
          this.options.runnerIds.includes(grant.runner_id) &&
          [grant.sub, grant.runner_id, grant.claim_id, grant.group_id, grant.project_id, grant.agent_id, grant.agent_version, grant.jti]
            .every((id) => typeof id === 'string' && id.length > 0) &&
          Number.isSafeInteger(grant.generation) && grant.generation >= 0 &&
          grant.credential_id === undefined && grant.credential_version === undefined && grant.delivery === undefined) {
        return grant;
      }
    } catch {
      // Verification failures have a fixed response.
    }
    throw new HttpError(401, 'capability.invalid', 'the step capability is invalid');
  }
}

/** Stable sandbox identity for one signed claim attempt, including across lease renewals. */
export function sandboxIdForClaim(claims: Pick<GrantClaims, 'runner_id' | 'sub' | 'claim_id' | 'generation'>): string {
  return 'rb-' + createHash('sha256')
    .update(JSON.stringify([claims.runner_id, claims.sub, claims.claim_id, claims.generation])).digest('hex');
}
