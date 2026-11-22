import { isDeepStrictEqual } from 'node:util';
import { sandboxIdForClaim } from '@runbook/runner-shared/capability';
import type { GrantClaims } from '@runbook/runner-shared/contracts/core';
import type { ClaimFence, MaterialPush } from '@runbook/runner-shared/contracts/internal';

export class MaterialStore {
  private readonly entries = new Map<string, { release: MaterialPush; timer: NodeJS.Timeout; abort: AbortController }>();
  private readonly revocations = new Map<string, NodeJS.Timeout>();
  private readonly ended = new Map<string, NodeJS.Timeout>();
  private readonly steps = new Map<string, Set<string>>();

  private readonly now: () => number;

  constructor(now: () => number = Date.now) {
    this.now = now;
  }

  get size(): number {
    return this.entries.size;
  }

  put(release: MaterialPush): void {
    if (release.claim_id !== undefined && release.generation !== undefined && release.runner_id !== undefined &&
        this.ended.has(sandboxIdForClaim({ sub: release.step_run_id, claim_id: release.claim_id,
          generation: release.generation, runner_id: release.runner_id }))) throw new RangeError('claim ended');
    if (this.revocations.has(release.credential_id)) throw new RangeError('credential revoked');
    const ttl = Date.parse(release.expires_at) - this.now();
    if (!Number.isFinite(ttl) || ttl <= 0 || ttl > 630_000) throw new RangeError('invalid release expiry');
    const previous = this.get(release.release_id);
    let abort = new AbortController();
    if (previous && Date.parse(release.expires_at) >= Date.parse(previous.expires_at) &&
        isDeepStrictEqual({ ...previous, expires_at: release.expires_at }, release)) {
      const entry = this.entries.get(release.release_id)!;
      clearTimeout(entry.timer);
      abort = entry.abort;
    } else {
      this.delete(release.release_id);
    }
    const timer = setTimeout(() => this.delete(release.release_id), ttl).unref();
    this.entries.set(release.release_id, { release, timer, abort });
    const ids = this.steps.get(release.step_run_id) ?? new Set<string>();
    ids.add(release.release_id);
    this.steps.set(release.step_run_id, ids);
  }

  get(id: string): MaterialPush | undefined {
    const release = this.entries.get(id)?.release;
    if (release && Date.parse(release.expires_at) <= this.now()) {
      this.delete(id);
      return undefined;
    }
    return release;
  }

  signal(release: MaterialPush): AbortSignal {
    const stored = this.get(release.release_id);
    if (stored !== release) throw new RangeError('release unavailable');
    return this.entries.get(release.release_id)!.abort.signal;
  }

  active(claims: GrantClaims): boolean {
    return claims.kind === 'capability' && claims.exp * 1000 > this.now() && !this.ended.has(sandboxIdForClaim(claims));
  }

  endClaim(claims: ClaimFence): void {
    const id = sandboxIdForClaim(claims);
    for (const releaseId of this.steps.get(claims.sub) ?? []) {
      const release = this.get(releaseId);
      if (release?.claim_id === claims.claim_id && release.generation === claims.generation &&
          release.runner_id === claims.runner_id) this.delete(releaseId);
    }
    clearTimeout(this.ended.get(id));
    this.ended.set(id, setTimeout(() => this.ended.delete(id), 630_000).unref());
  }

  forStep(claims: GrantClaims): MaterialPush[] {
    const releases: MaterialPush[] = [];
    if (!this.active(claims)) return releases;
    for (const id of this.steps.get(claims.sub) ?? []) {
      const release = this.get(id);
      if (release && release.claim_id === claims.claim_id && release.generation === claims.generation &&
          release.runner_id === claims.runner_id) releases.push(release);
    }
    return releases;
  }

  delete(id: string): boolean {
    const entry = this.entries.get(id);
    if (!entry) return false;
    clearTimeout(entry.timer);
    const step = entry.release.step_run_id;
    const ids = this.steps.get(step);
    ids?.delete(id);
    if (ids?.size === 0) this.steps.delete(step);
    this.entries.delete(id);
    entry.abort.abort();
    return true;
  }

  revokeCredential(id: string): void {
    clearTimeout(this.revocations.get(id));
    // Fence any live grant that could have been issued before revocation.
    this.revocations.set(id, setTimeout(() => this.revocations.delete(id), 630_000).unref());
    for (const { release } of this.entries.values()) {
      if (release.credential_id === id) this.delete(release.release_id);
    }
  }

  clear(): void {
    for (const timer of this.revocations.values()) clearTimeout(timer);
    this.revocations.clear();
    for (const timer of this.ended.values()) clearTimeout(timer);
    this.ended.clear();
    for (const id of this.entries.keys()) this.delete(id);
  }
}
