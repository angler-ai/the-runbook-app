import type { Config } from '@runbook/runner-shared/config';
import type { SandboxSize } from '@runbook/runner-shared/contracts/internal';
import { HttpError } from '@runbook/runner-shared/http';
import { SIZES } from './template.ts';

export interface ComputeRates {
  vcpu_per_second_usd: number;
  gib_per_second_usd: number;
}

export function meterSandbox(size: SandboxSize, rates: ComputeRates, elapsedMs: number) {
  const seconds = Math.max(0, elapsedMs) / 1000;
  const usage = { vcpu_seconds: SIZES[size].vcpu * seconds, gib_seconds: SIZES[size].gib * seconds };
  return { amount: usage.vcpu_seconds * rates.vcpu_per_second_usd + usage.gib_seconds * rates.gib_per_second_usd, usage };
}

export function computeClient(config: Config) {
  const unavailable = () => new HttpError(503, 'sandbox.ledger_unavailable', 'compute ledger unavailable');
  async function post(path: string, body: object) {
    try {
      return await fetch(`${config.urls.api}/internal/${path}`, {
        method: 'POST', headers: { 'content-type': 'application/json', authorization: `Bearer ${config.outboundTokens.api}` },
        body: JSON.stringify(body), redirect: 'error', signal: AbortSignal.timeout(5000),
      });
    } catch {
      throw unavailable();
    }
  }
  return {
    async reserve(capability: string, size: SandboxSize) {
      const reply = await post('compute/rates', { capability });
      if (!reply.ok) throw unavailable();
      const rates = await reply.json() as ComputeRates & { timeout_s: number };
      if ([rates.vcpu_per_second_usd, rates.gib_per_second_usd].some((rate) => !Number.isFinite(rate) || rate < 0) ||
          !Number.isSafeInteger(Math.round(rates.timeout_s * 1000)) || rates.timeout_s < 0.001 || rates.timeout_s > 86400) throw unavailable();
      const reservation = await post('ledger/reserve', {
        capability, amount_usd: meterSandbox(size, rates, rates.timeout_s * 1000).amount, kind: 'compute',
      });
      const body = await reservation.json() as { reservation_id?: string; level?: string };
      if (reservation.status === 402 && typeof body.level === 'string' && /^[a-z_]{1,30}$/.test(body.level)) {
        return { level: body.level } as const;
      }
      if (!reservation.ok || typeof body.reservation_id !== 'string') throw unavailable();
      const reservationId = body.reservation_id;
      return {
        timeoutMs: Math.round(rates.timeout_s * 1000),
        async settle(elapsedMs: number) {
          const metered = meterSandbox(size, rates, elapsedMs);
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
