import { traceFetch } from '@runbook/runner-shared/telemetry';
import { DECISION_CONTRACT } from '@runbook/runner-shared/contracts/core';
import { randomUUID } from 'node:crypto';
import type { Config } from '@runbook/runner-shared/config';
import type { AdmissionResponse, CapabilityResponse } from '@runbook/runner-shared/contracts/internal';
import { CoreError, coreJson, type ClaimResponse, type components } from '@runbook/runner-shared/core-client';
import { type ClaimDependencies } from './claim.ts';
import { StepStopped } from './loop/limits.ts';

type Process = 'api' | 'vault' | 'gateway' | 'sandbox-ctl';

export class InternalError extends Error {
  readonly status: number;
  readonly level: string | undefined;
  readonly code: string | undefined;
  readonly budgetDetail: string | undefined;
  constructor(status: number, level?: string, code?: string, budgetDetail?: string) {
    super('runner request failed');
    this.status = status;
    this.level = level;
    this.code = code;
    this.budgetDetail = budgetDetail;
  }
}

export function workerClients(config: Config, fetcher = fetch) {
  const core = { baseUrl: config.coreUrl!, token: config.coreToken!, fetch: fetcher };
  const runner = config.runnerId!;
  const path = (step: string, action = '') => `/v1/step-runs/${encodeURIComponent(step)}${action}`;

  async function request<T>(process: Process, route: string, options: {
    method?: 'GET' | 'POST' | 'PUT' | 'DELETE'; body?: unknown; signal?: AbortSignal;
    capability?: string; ignoreMissing?: boolean; timeoutMs?: number;
  } = {}): Promise<T> {
    const { method = 'POST', capability, body, signal, ignoreMissing, timeoutMs = 8000 } = options;
    const timeout = AbortSignal.timeout(timeoutMs);
    const response = await traceFetch(fetcher, config.urls[process] + route, {
      method, headers: {
        authorization: `Bearer ${config.outboundTokens[process]}`,
        ...(capability && { 'x-runbook-capability': capability }),
        ...(body !== undefined && { 'content-type': 'application/json' }),
      },
      ...(body !== undefined && { body: JSON.stringify(body) }), redirect: 'error',
      signal: signal ? AbortSignal.any([signal, timeout]) : timeout,
    });
    if (response.status === 204 || (ignoreMissing && response.status === 404)) return undefined as T;
    if (!response.ok) {
      const result = await response.json().catch(() => null) as { level?: unknown; budget?: { detail?: unknown }; error?: { code?: unknown } } | null;
      const level = result?.level;
      const code = result?.error?.code;
      const detail = result?.budget?.detail;
      const budgetDetail = process === 'sandbox-ctl' && response.status === 402 &&
        typeof detail === 'string' && detail.length <= 240 && [...detail].every((char) => {
          const code = char.charCodeAt(0);
          return code >= 32 && (code < 127 || code > 159);
        }) ? detail : undefined;
      throw new InternalError(response.status,
        typeof level === 'string' && /^[a-z][a-z0-9_:.-]{0,30}$/.test(level) ? level : undefined,
        typeof code === 'string' && /^[a-z][a-z0-9_.-]{0,63}$/.test(code) ? code : undefined, budgetDetail);
    }
    return await response.json() as T;
  }

  const deps: Omit<ClaimDependencies, 'execute'> = {
    claim: (step, signal) => coreJson<ClaimResponse>({ ...core, signal, idempotencyKey: randomUUID() },
      'POST', path(step, '/claim'), { session_id: `runner:${runner}`, actor: { kind: 'agent', agent_name: runner } }),
    heartbeat: (step, claim, signal) => coreJson({ ...core, signal, idempotencyKey: randomUUID() },
      'POST', path(step, '/heartbeat'), { claim }),
    async capability(step, claim, signal) {
      const answer = await request<CapabilityResponse>('vault', '/internal/capability', { signal,
        body: { step_run_id: step, claim_id: claim.id, generation: claim.generation, runner_id: runner } });
      if (typeof answer.capability !== 'string' || !answer.capability) throw new InternalError(502);
      const expiresAt = Date.parse(answer.expires_at);
      if (!Number.isFinite(expiresAt) || expiresAt <= Date.now()) throw new InternalError(502);
      return { token: answer.capability, expiresAt };
    },
    refresh: (slot, capability, signal) => request('api', `/internal/slots/${encodeURIComponent(slot)}/heartbeat`,
      { body: { capability }, signal }),
    cost: (slot, capability, signal) => request<components['schemas']['StepCost']>(
      'api', `/internal/slots/${encodeURIComponent(slot)}/cost`, { body: { capability }, signal }),
    release: (slot, outcome) => request('api', `/internal/slots/${encodeURIComponent(slot)}/release`,
      { ...(outcome && { body: { outcome } }), ignoreMissing: true }),
    endClaim: (capability) => request('vault', '/internal/end-claim', { body: { capability }, timeoutMs: 5000 }),
    renewSandbox: (id, capability, signal) => request('sandbox-ctl', `/internal/sandboxes/${id}/renew`, { capability, signal }),
    destroySandbox: (id, capability) => request('sandbox-ctl', `/internal/sandboxes/${id}`,
      { method: 'DELETE', capability, ignoreMissing: true }),
    report: (step, claim, outcome, signal, idempotencyKey) => coreJson({ ...core, signal, idempotencyKey },
      'POST', path(step, '/report'), { ...outcome, step_run_id: step, claim }),
    finishDecision: async (step, claim, outcome, signal, idempotencyKey) => {
      const finish = outcome.decision ?? { reason: outcome.summary || 'Could not complete review.' };
      const action = 'option' in finish ? 'decide' : 'abstain';
      const query = new URLSearchParams({ step_run_id: step });
      await coreJson({ ...core, signal, idempotencyKey }, 'POST',
        `/v1/runner/decisions/${encodeURIComponent(claim.id)}/${action}?${query}`,
        finish);
    },
    abandon: (step, claim) => coreJson({ ...core, idempotencyKey: randomUUID() }, 'POST', path(step, '/release'), { claim }),
  };
  return {
    ...deps,
    admit: (free_slots: number) => request<AdmissionResponse>('api', '/internal/admission', { body: { free_slots, decision_contract: DECISION_CONTRACT } }),
    request,
    logs: (step: string, claim: Parameters<ClaimDependencies['report']>[1], logs: string[], signal: AbortSignal) =>
      coreJson({ ...core, signal, idempotencyKey: randomUUID() }, 'POST', path(step, '/logs'), { claim, logs }),
    checklist: (step: string, claim: Parameters<ClaimDependencies['report']>[1], titles: string[], signal: AbortSignal) =>
      coreJson({ ...core, signal, idempotencyKey: randomUUID() }, 'POST', path(step, '/checklist'), {
        claim, checklist: titles.map((title) => ({ title, completed: true })),
      }),
    context: async <T>(step: string, claim: Parameters<ClaimDependencies['report']>[1],
      action: 'run' | 'step' | 'runs' | 'past-run' | 'memory', signal: AbortSignal,
      query: Record<string, string> = {}, proposal?: components['schemas']['RunnerMemoryProposal']) => {
      const params = new URLSearchParams({ ...query, claim_id: claim.id, generation: String(claim.generation) });
      const route = path(step, `/context/${action}?${params}`);
      try {
        return await coreJson<T>({ ...core, signal, ...(proposal && { idempotencyKey: randomUUID() }) },
          proposal ? 'POST' : 'GET', route, proposal);
      } catch (error) {
        if (error instanceof CoreError) {
          if ([401, 403].includes(error.status)) throw new StepStopped('claim_context_refused');
          if (error.status === 409 && error.code === 'memory.proposal_limit') return { error: error.code } as T;
        }
        throw error;
      }
    },
    step: <T>(step: string, signal: AbortSignal) => coreJson<T>({ ...core, signal }, 'GET', path(step)),
  };
}
