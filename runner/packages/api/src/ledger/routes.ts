import { createHash, randomUUID } from 'node:crypto';
import type pg from 'pg';
import type { CapabilityVerifier } from '@runbook/runner-shared/capability';
import { HttpError, type Route } from '@runbook/runner-shared/http';
import type { ReserveRequest, SettleRequest } from '@runbook/runner-shared/contracts/internal';
import { withTransaction } from '@runbook/runner-shared/db';
import { CapExceeded, reserveInTransaction } from './reserve.ts';
import { configuredCaps, type AgentLimits, type BudgetPolicy } from './limits.ts';
import { settle } from './settle.ts';

interface ContextRow extends BudgetPolicy {
  slot_id: string;
  execution_id: string;
  max_cost_usd: string;
  time_zone: string;
  agent_limits: AgentLimits | null;
}

export function ledgerRoutes(pool: pg.Pool, verifier: CapabilityVerifier, orgId: string,
  wakeAlerts: () => void): Route[] {
  return [
    { method: 'POST', path: '/internal/ledger/reserve', callers: ['gateway', 'sandbox-ctl'],
      async handle({ body, caller }) {
        const request = body as ReserveRequest | undefined;
        if (!request || typeof request.capability !== 'string' || typeof request.amount_usd !== 'number' ||
            (request.kind !== 'model' && request.kind !== 'compute') ||
            (request.minimum_usd !== undefined && (request.kind !== 'model' ||
              !Number.isFinite(request.minimum_usd) || request.minimum_usd < 0 || request.minimum_usd > request.amount_usd)) ||
            (request.headroom_usd !== undefined && (request.minimum_usd === undefined ||
              !Number.isFinite(request.headroom_usd) || request.headroom_usd < 0))) {
          throw new HttpError(400, 'ledger.invalid_request', 'invalid reservation request');
        }
        if ((caller === 'sandbox-ctl') !== (request.kind === 'compute')) {
          throw new HttpError(403, 'ledger.kind_denied', 'reservation kind unavailable');
        }
        const claims = await verifier.verifyCapability(request.capability);
        try {
          const result = await withTransaction(pool, async (client) => {
            const { rows: [context] } = await client.query<ContextRow>(
              `SELECT s.id AS slot_id, s.execution_id, s.max_cost_usd, p.time_zone, p.org_cost_per_day_usd, p.org_cost_per_month_usd,
                 p.project_limits, p.run_max_cost_usd, v.config->'limits' AS agent_limits,
                 g.cost_per_day_usd, g.cost_per_month_usd
               FROM slots s JOIN groups g ON g.group_id = s.group_id
               JOIN agent_versions v ON v.id = $6 AND v.agent_id = s.agent_id CROSS JOIN policy p
               WHERE s.step_run_id = $1 AND s.runner_id = $2 AND s.group_id = $3 AND s.project_id = $4 AND s.agent_id = $5
                 AND s.released_at IS NULL AND s.metering_closed_at IS NULL AND s.expires_at > now()
                 AND s.claim_id = $7 AND s.claim_generation = $8 FOR UPDATE OF s`,
              [claims.sub, claims.runner_id, claims.group_id, claims.project_id, claims.agent_id, claims.agent_version, claims.claim_id, claims.generation],
            );
            if (!context?.execution_id || context.max_cost_usd === null) {
              throw new HttpError(403, 'ledger.slot_unavailable', 'active reservation context unavailable');
            }
            const reservation = await reserveInTransaction(client, {
              orgId, groupId: claims.group_id, projectId: claims.project_id, agentId: claims.agent_id,
              runnerId: claims.runner_id, runId: context.execution_id, stepId: claims.sub, timeZone: context.time_zone,
              caps: configuredCaps(context, claims.project_id, context.agent_limits ?? undefined, Number(context.max_cost_usd)),
            }, request.amount_usd, request.kind, new Date(), request.minimum_usd, request.headroom_usd);
            await client.query('UPDATE ledger_reservations SET capability_hash = $2, decision_lease_id = $3, slot_id = $4 WHERE id = $1',
              [reservation.reservation_id, createHash('sha256').update(request.capability).digest(), claims.reach === 'read' ? claims.claim_id : null, context.slot_id]);
            return reservation;
          });
          return { body: result };
        } catch (error) {
          if (error instanceof CapExceeded) return { status: 402, body: { level: error.level } };
          throw error;
        }
      } },
    { method: 'POST', path: '/internal/ledger/settle', callers: ['gateway', 'sandbox-ctl'],
      async handle({ body, caller }) {
        const request = body as (SettleRequest & { capability?: unknown }) | undefined;
        if (!request || (request.capability !== undefined &&
            (typeof request.capability !== 'string' || !request.capability)) || typeof request.reservation_id !== 'string' || typeof request.actual_usd !== 'number' ||
            !request.usage || typeof request.usage !== 'object' || Array.isArray(request.usage)) {
          throw new HttpError(400, 'ledger.invalid_request', 'invalid settlement request');
        }
        // The same capability was verified and bound to this reservation at
        // dispatch. Settlement must remain available after the lease ends.
        const digest = request.capability === undefined ? null : createHash('sha256').update(request.capability).digest();
        const alerts = await settle(pool, request, caller === 'sandbox-ctl' ? 'compute' : 'model', digest);
        if (alerts.length) wakeAlerts();
        return { status: 204 };
      } },
    { method: 'POST', path: '/internal/compute/rates', callers: ['sandbox-ctl'],
      async handle({ body }) {
        const capability = (body as { capability?: unknown } | null)?.capability;
        if (typeof capability !== 'string') throw new HttpError(401, 'capability.invalid', 'the step capability is invalid');
        const claims = await verifier.verifyCapability(capability);
        const { rows: [row] } = await pool.query<{ compute_rate: Record<string, unknown>; timeout_s: unknown }>(
          `SELECT g.compute_rate, v.config->'limits'->'timeout_s' AS timeout_s FROM groups g
           JOIN agents a ON a.group_id = g.group_id JOIN agent_versions v ON v.agent_id = a.id
           WHERE g.group_id = $1 AND a.id = $2 AND a.project_id = $3 AND v.id = $4 AND a.status = 'active'`,
          [claims.group_id, claims.agent_id, claims.project_id, claims.agent_version],
        );
        const remaining = claims.step_deadline_ms == null ? Infinity : claims.step_deadline_ms - Date.now();
        if (remaining <= 0 || !row || typeof row.timeout_s !== 'number') throw new HttpError(403, 'ledger.compute_unavailable', 'compute context unavailable');
        return { body: { vcpu_per_second_usd: row.compute_rate.vcpu_per_second_usd ?? 0,
          gib_per_second_usd: row.compute_rate.gib_per_second_usd ?? 0, timeout_s: Math.min(row.timeout_s, (claims.timeout_ms ?? Infinity) / 1000, remaining / 1000) } };
      } },
    { method: 'POST', path: '/internal/compute/probe-alert', callers: ['sandbox-ctl'],
      async handle({ body }) {
        const capability = (body as { capability?: unknown } | null)?.capability;
        if (typeof capability !== 'string') throw new HttpError(401, 'capability.invalid', 'the step capability is invalid');
        const claims = await verifier.verifyCapability(capability);
        const id = randomUUID();
        await pool.query("INSERT INTO budget_event_outbox(id, group_id, runner_id, data, kind) VALUES ($1,$2,$3,$4,'runner.probe_succeeded')",
          [id, claims.group_id, claims.runner_id, JSON.stringify({ event_id: id, step_run_id: claims.sub,
            group_id: claims.group_id, runner_id: claims.runner_id })]);
        wakeAlerts();
        return { status: 204 };
      } },
  ];
}
