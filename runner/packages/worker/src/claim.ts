import type { SlotOutcome } from '@runbook/runner-shared/contracts/internal';
import { randomUUID } from 'node:crypto';
import { CoreError, type components, type ReportRequest } from '@runbook/runner-shared/core-client';
import { HEARTBEAT_MS, sleep, superviseLease, whileActive } from './lease.ts';

export interface Admission { step_run_id: string; slot_id: string; kind?: 'step' | 'decision' }
type Claim = components['schemas']['ClaimRef'];
export type DecisionFinish = { option: string; comment: string; evidence: string[] } | { reason: string };
export type Outcome = Omit<ReportRequest, 'claim' | 'step_run_id'> & {
  decision?: DecisionFinish;
};

export interface ExecutionContext {
  stepRunId: string;
  kind?: Admission['kind'];
  claim: Claim;
  signal: AbortSignal;
  capability(): string;
  registerSandbox(id: string): void;
  sandboxStarted(): void;
}

export interface ClaimDependencies {
  claim(step: string, signal: AbortSignal): Promise<Claim>;
  heartbeat(step: string, claim: Claim, signal: AbortSignal): Promise<void>;
  capability(step: string, claim: Claim, signal: AbortSignal): Promise<{ token: string; expiresAt: number }>;
  refresh(slot: string, capability: string, signal: AbortSignal): Promise<void>;
  renewSandbox(id: string, capability: string, signal: AbortSignal): Promise<void>;
  destroySandbox(id: string, capability: string): Promise<void>;
  release(slot: string, outcome?: SlotOutcome): Promise<void>;
  cost?(slot: string, capability: string, signal: AbortSignal): Promise<components['schemas']['StepCost']>;
  report(step: string, claim: Claim, outcome: Outcome, signal: AbortSignal, idempotencyKey: string): Promise<void>;
  finishDecision?(step: string, claim: Claim, outcome: Outcome, signal: AbortSignal, idempotencyKey: string): Promise<void>;
  abandon(step: string, claim: Claim): Promise<void>;
  execute(ctx: ExecutionContext): Promise<Outcome>;
  heartbeatMs?: number;
  endClaim?(capability: string): Promise<void>;
  onSlotReleaseFailure?(): void;
}

function staleClaim(error: unknown) {
  const { code } = (error ?? {}) as { code?: string };
  return ['claim.stale_generation', 'claim.not_found', 'grant.claim_moved', 'grant.lease_expired'].includes(code ?? '');
}

export async function runClaim(item: Admission, deps: ClaimDependencies, shutdown: AbortSignal): Promise<boolean> {
  const { step_run_id: step, slot_id: slot } = item;
  let claim: Claim | undefined;
  let capability: string | undefined;
  let expiresAt = 0;
  let sandbox: string | undefined;
  let sandboxReady = false;
  let lost = false;
  let executed = false;
  let reported: SlotOutcome | undefined;
  let stop: (() => Promise<void>) | undefined;
  const lease = new AbortController();
  const signal = AbortSignal.any([shutdown, lease.signal]);
  async function destroy() {
    sandboxReady = false;
    const id = sandbox;
    if (!id || !capability) return true;
    for (let attempt = 0; attempt < 3; attempt++) {
      try {
        await deps.destroySandbox(id, capability);
        sandbox = undefined;
        return true;
      } catch {
        if (attempt < 2) await sleep(250);
      }
    }
    // The controller retains the sandbox in its reaper until teardown succeeds.
    return false;
  }
  try {
    const held = await deps.claim(step, signal);
    claim = held;
    const refresh = async (tick: AbortSignal) => {
      const active = AbortSignal.any([tick, lease.signal]);
      const next = await deps.capability(step, held, active);
      active.throwIfAborted();
      // Keep the freshest grant for cleanup even if renewing the slot fails.
      capability = next.token;
      await deps.refresh(slot, next.token, active);
      if (sandbox && sandboxReady) await deps.renewSandbox(sandbox, next.token, active);
      active.throwIfAborted();
      expiresAt = next.expiresAt;
    };
    await refresh(signal);
    stop = superviseLease(deps.heartbeatMs ?? HEARTBEAT_MS, () => expiresAt, lease.signal, async (tick) => {
      await deps.heartbeat(step, held, tick);
      await refresh(tick);
    }, staleClaim, () => { lost = true; lease.abort(); });
    const outcome = await whileActive(signal, async () => {
      const result = await deps.execute({
        stepRunId: step, kind: item.kind, claim: held, signal, capability: () => capability!,
        registerSandbox(id) { signal.throwIfAborted(); sandbox = id; },
        sandboxStarted() { signal.throwIfAborted(); sandboxReady = true; },
      });
      executed = true;
      return result;
    });
    const destroyed = await destroy();
    const cost = item.kind !== 'decision' && destroyed && capability && deps.cost
      ? await deps.cost(slot, capability, lease.signal).catch(() => undefined) : undefined;
    let report = { ...outcome, ...(cost && { cost }) };
    let idempotencyKey = randomUUID();
    let fellBack = false;
    while (!lease.signal.aborted) {
      try {
        if (item.kind === 'decision') {
          if (!deps.finishDecision) return false;
          await deps.finishDecision(step, held, report, lease.signal, idempotencyKey);
        } else {
          await deps.report(step, held, report, lease.signal, idempotencyKey);
        }
        reported = { claim_id: held.id, generation: held.generation, status: report.status,
          idle_stop: report.status === 'failed' && outcome.summary === 'idle' };
        return true;
      } catch (error) {
        if (error instanceof CoreError) {
          if (item.kind === 'decision' && error.status === 409 &&
              ['approval.not_open', 'approval.deadline_exceeded'].includes(error.code)) return true;
          if (staleClaim(error)) return false;
          // A task report the core refuses for good (not a stale claim, a
          // timeout or a rate limit) is replaced once by a minimal failed
          // report, so the step ends instead of holding its claim forever.
          const permanent = error.status >= 400 && error.status < 500 && ![408, 429].includes(error.status);
          if (item.kind !== 'decision' && permanent) {
            if (fellBack) return false;
            fellBack = true;
            report = { status: 'failed', summary: `report_rejected:${error.code}`, outputs: {}, evidence: [], logs: [],
              ...(cost && { cost }) };
            idempotencyKey = randomUUID();
            continue;
          }
          if (item.kind === 'decision' && error.status === 422) {
            if (!report.decision || !('option' in report.decision)) return false;
            report = { ...report, status: 'blocked',
              decision: { reason: 'The runner decision was rejected. Waiting for a person.' } };
            idempotencyKey = randomUUID();
          }
        }
        await sleep(1000, lease.signal);
      }
    }
    return false;
  } catch {
    return false;
  } finally {
    // Stop model and tool work before issuing any teardown requests.
    lease.abort();
    await stop?.();
    if (capability && deps.endClaim) await deps.endClaim(capability).catch(() => undefined);
    const sandboxStopped = await destroy();
    if (claim && !lost && !executed) await deps.abandon(step, claim).catch(() => undefined);
    if (sandboxStopped) {
      for (let attempt = 0; attempt < 3; attempt++) {
        try {
          await deps.release(slot, reported);
          break;
        } catch {
          if (attempt < 2) await sleep(250 * 2 ** attempt);
          else deps.onSlotReleaseFailure?.();
        }
      }
    }
  }
}
