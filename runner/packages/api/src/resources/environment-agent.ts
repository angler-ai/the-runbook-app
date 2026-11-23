import type pg from 'pg';
import { HttpError } from '@runbook/runner-shared/http';
import { validateEgress, type AgentConfig } from './agents.ts';
import { environmentPin, sizes } from './environments.ts';

export async function applyEnvironment(db: pg.PoolClient, org: string, input: Record<string, unknown>,
  current: AgentConfig | null | undefined, config: AgentConfig, preservePin: boolean) {
  const id = Object.hasOwn(input, 'environment_id') ? input.environment_id : current?.environment_id;
  const invalid = (field: string): never => {
    throw new HttpError(422, 'agent.environment_limit', `${field} exceeds the environment defaults`);
  };
  if (id != null && (typeof id !== 'string' || !/^[a-zA-Z0-9_-]{1,128}$/.test(id)))
    throw new HttpError(422, 'agent.environment_unavailable', 'invalid environment_id');
  // A project editor can save instructions, but cannot adopt a more
  // permissive environment revision without the agent publish permission.
  const pin = await environmentPin(db, org, id as string | null | undefined,
    preservePin ? current?.environment_version_id : undefined, preservePin && !current?.environment_version_id);
  if (!pin) {
    if (id != null || current?.environment_version_id) {
      throw new HttpError(422, 'agent.environment_unavailable', 'environment unavailable');
    }
    return;
  }
  const { config: env } = pin;
  const supplied = (input.limits ?? {}) as Record<string, unknown>;
  const previous = current?.environment_overrides ?? (current?.environment_version_id ? {} : {
    size: current?.sandbox?.size, timeout_s: current?.limits.timeout_s, egress: current?.egress,
    max_cost_usd: current?.limits.max_cost_usd, max_run_cost_usd: current?.limits.max_run_cost_usd ?? undefined,
  });
  const size = Object.hasOwn(input, 'sandbox') ? config.sandbox?.size : previous.size;
  const timeout_s = Object.hasOwn(supplied, 'timeout_s') ? config.limits.timeout_s ?? undefined : previous.timeout_s;
  const max_cost_usd = Object.hasOwn(supplied, 'max_cost_usd') ? config.limits.max_cost_usd ?? undefined : previous.max_cost_usd;
  const max_run_cost_usd = 'limits' in input ? config.limits.max_run_cost_usd ?? undefined : previous.max_run_cost_usd;
  const egress = Object.hasOwn(input, 'egress') ? input.egress === null ? undefined : validateEgress(config.egress) : previous.egress;
  if (size && sizes.indexOf(size) > sizes.indexOf(env.size)) invalid('sandbox.size');
  if (timeout_s !== undefined && timeout_s > env.timeout_s) invalid('limits.timeout_s');
  if (env.budgets.max_step_cost_usd != null && max_cost_usd !== undefined && max_cost_usd > env.budgets.max_step_cost_usd)
    invalid('limits.max_cost_usd');
  if (env.budgets.max_run_cost_usd != null && max_run_cost_usd != null && max_run_cost_usd > env.budgets.max_run_cost_usd)
    invalid('limits.max_run_cost_usd');
  if (env.egress && egress?.some((entry) => !env.egress?.some((rule) =>
    rule.host === entry.host && (rule.label === 'write' || entry.label === 'read')))) invalid('egress');
  for (const credential_id of new Set(Object.values(env.credential_env))) {
    if (!config.credentials.some((item) => item.credential_id === credential_id && item.delivery === 'proxy')) {
      throw new HttpError(422, 'agent.environment_credential_required', 'select the environment credentials with proxy delivery');
    }
  }
  config.environment_id = pin.id;
  config.environment_version_id = pin.version_id;
  config.environment_overrides = { size, timeout_s, egress, max_cost_usd, max_run_cost_usd };
  config.sandbox = { size: size ?? env.size };
  config.egress = egress ?? env.egress ?? [];
  config.limits = { ...config.limits, timeout_s: timeout_s ?? env.timeout_s,
    max_cost_usd: max_cost_usd ?? env.budgets.max_step_cost_usd ?? config.limits.max_cost_usd ?? 1,
    max_run_cost_usd: max_run_cost_usd ?? env.budgets.max_run_cost_usd ?? undefined };
}
