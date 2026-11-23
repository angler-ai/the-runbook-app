import { fetchWithTrace } from '@runbook/runner-shared/telemetry';
import type { SandboxEnvironment } from '@runbook/runner-shared/contracts/environments';
import type { Config } from '@runbook/runner-shared/config';
import type { SandboxSize } from '@runbook/runner-shared/contracts/internal';
import { HttpError } from '@runbook/runner-shared/http';
import { SIZES } from './template.ts';

// Leave a bounded allowance for the runtime deadline's rounding and teardown.
export const CLEANUP_ALLOWANCE_MS = 10_000;

export interface ComputeRates {
  vcpu_per_second_usd: number;
  gib_per_second_usd: number;
}

export function meterSandbox(size: SandboxSize, rates: ComputeRates, elapsedMs: number, resources: { vcpu: number; gib: number } = SIZES[size]) {
  const seconds = Math.max(0, elapsedMs) / 1000;
  const usage = { vcpu_seconds: resources.vcpu * seconds, gib_seconds: resources.gib * seconds };
  return { amount: usage.vcpu_seconds * rates.vcpu_per_second_usd + usage.gib_seconds * rates.gib_per_second_usd, usage };
}

export function computeClient(config: Config) {
  const unavailable = () => new HttpError(503, 'sandbox.ledger_unavailable', 'compute ledger unavailable');
  async function post(path: string, body: object) {
    try {
      return await fetchWithTrace(`${config.urls.api}/internal/${path}`, {
        method: 'POST', headers: { 'content-type': 'application/json', authorization: `Bearer ${config.outboundTokens.api}` },
        body: JSON.stringify(body), redirect: 'error', signal: AbortSignal.timeout(5000),
      });
    } catch {
      throw unavailable();
    }
  }
  return {
    async seedEnvironment(image: string) {
      if (!(await post('environment/default', { image })).ok) throw unavailable();
    },
    async environment(capability: string) {
      const reply = await post('environment/sandbox', { capability });
      if (reply.status === 204) return undefined;
      if (reply.status === 403) throw new HttpError(403, 'sandbox.image_not_approved', 'sandbox image is outside the approved list');
      if (!reply.ok) throw unavailable();
      return await reply.json() as SandboxEnvironment;
    },
    async rates(capability: string) {
      const reply = await post('compute/rates', { capability });
      if (!reply.ok) throw unavailable();
      const rates = await reply.json() as ComputeRates & { timeout_s: number };
      if ([rates.vcpu_per_second_usd, rates.gib_per_second_usd].some((rate) => !Number.isFinite(rate) || rate < 0) ||
          !Number.isSafeInteger(Math.round(rates.timeout_s * 1000)) || rates.timeout_s < 0.001 || rates.timeout_s > 86400) throw unavailable();
      return rates;
    },
    async reserve(capability: string, size: SandboxSize, rates: ComputeRates & { timeout_s: number },
      resources?: { vcpu: number; gib: number }) {
      const timeoutMs = Math.floor(rates.timeout_s * 1000);
      const reservation = await post('ledger/reserve', {
        capability, amount_usd: meterSandbox(size, rates, timeoutMs + CLEANUP_ALLOWANCE_MS, resources).amount, kind: 'compute',
      });
      const body = await reservation.json() as { reservation_id?: string; level?: string; budget?: Record<string, unknown> };
      if (reservation.status === 402 && typeof body.level === 'string' && /^[a-z_]{1,30}$/.test(body.level)) {
        return { level: body.level, ...(body.budget && { budget: body.budget }) } as const;
      }
      if (!reservation.ok || typeof body.reservation_id !== 'string') throw unavailable();
      const reservationId = body.reservation_id;
      return {
        timeoutMs,
        async settle(elapsedMs: number, admitted = resources) {
          const metered = meterSandbox(size, rates, elapsedMs, admitted);
          const response = await post('ledger/settle', {
            capability, reservation_id: reservationId, actual_usd: metered.amount, usage: metered.usage,
          });
          if (!response.ok) throw unavailable();
        },
      } as const;
    },
    async sandbox(id: string, phase: 'started' | 'ready' | 'stopped', at = Date.now()) {
      if (!(await post(`sandboxes/${id}/meter`, { phase, at })).ok) throw unavailable();
    },
    async releaseSlot(id: string) {
      if (!(await post(`slots/sandbox/${id}/release`, {})).ok) throw unavailable();
    },
    async alert(capability: string) {
      if (!(await post('compute/probe-alert', { capability })).ok) throw unavailable();
    },
  };
}

export type ComputeHold = Exclude<Awaited<ReturnType<ReturnType<typeof computeClient>['reserve']>>, { level: string }>;
