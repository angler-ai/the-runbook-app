import { fetchWithTrace } from '@runbook/runner-shared/telemetry';
import type { Config } from '@runbook/runner-shared/config';
import type { CapabilityVerifier } from '@runbook/runner-shared/capability';
import type { RunningSandbox } from './server.ts';

export const REAPER_INTERVAL_MS = 15_000;

export async function reapSandboxes(entries: Iterable<RunningSandbox>,
  verify: (entry: RunningSandbox) => Promise<boolean>, destroy: (entry: RunningSandbox) => Promise<void>,
  now = Date.now()): Promise<void> {
  const results = await Promise.allSettled([...entries].map(async (entry) => {
    if (entry.claims.exp * 1000 <= now || entry.fenced || entry.stopped !== undefined ||
        !await verify(entry).catch(() => true)) await destroy(entry);
  }));
  if (results.some(({ status }) => status === 'rejected')) throw new Error('sandbox teardown incomplete');
}

export async function refreshedGrant(config: Config, verifier: Pick<CapabilityVerifier, 'verifyCapability'>,
  claims: RunningSandbox['claims']) {
  const response = await fetchWithTrace(`${config.urls.vault}/internal/capability`, {
    method: 'POST', headers: { 'content-type': 'application/json', authorization: `Bearer ${config.outboundTokens.vault}` },
    body: JSON.stringify({ step_run_id: claims.sub, runner_id: claims.runner_id, claim_id: claims.claim_id,
      generation: claims.generation }), redirect: 'error', signal: AbortSignal.timeout(4000),
  });
  if (response.status >= 500 || response.status === 429) throw new Error('vault lease check unavailable');
  if (!response.ok) return undefined;
  const { capability } = await response.json() as { capability?: string };
  if (typeof capability !== 'string') return undefined;
  const next = await verifier.verifyCapability(capability).catch(() => undefined);
  if (next && next.sub === claims.sub && next.runner_id === claims.runner_id && next.claim_id === claims.claim_id &&
    next.generation === claims.generation && next.group_id === claims.group_id &&
    next.project_id === claims.project_id && next.agent_version === claims.agent_version) return { capability, claims: next };
}

/** A missing listener on a reachable gateway fences the sandbox for good. */
export async function gatewayListenerAlive(config: Config, entry: RunningSandbox, fetcher = fetch): Promise<boolean> {
  const response = await fetcher(`${config.urls.gateway}/internal/sandboxes/${entry.sandbox.id}/check`, {
    method: 'POST', headers: { 'content-type': 'application/json', authorization: `Bearer ${config.outboundTokens.gateway}` },
    body: JSON.stringify({ capability: entry.capability }), redirect: 'error', signal: AbortSignal.timeout(4000),
  });
  await response.body?.cancel();
  // Older gateways answer 404 for this route. A transport or auth error
  // also leaves the existing lease in force until expiry or renewal.
  return response.status !== 410;
}

export function liveGrant(config: Config, verifier: Pick<CapabilityVerifier, 'verifyCapability'>) {
  return async ({ claims }: RunningSandbox) => Boolean(await refreshedGrant(config, verifier, claims));
}

export function startReaper(entries: ReadonlyMap<string, RunningSandbox>, verify: (entry: RunningSandbox) => Promise<boolean>,
  destroy: (entry: RunningSandbox) => Promise<void>, onFailure: () => void, intervalMs = REAPER_INTERVAL_MS): () => void {
  let busy: Promise<void> | undefined;
  const timer = setInterval(() => {
    busy ??= reapSandboxes(entries.values(), verify, destroy).catch(onFailure).finally(() => { busy = undefined; });
  }, intervalMs).unref();
  return () => clearInterval(timer);
}
