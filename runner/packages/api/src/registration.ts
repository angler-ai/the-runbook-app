import type { JoinResponse, RegisterResponse } from '@runbook/runner-shared/contracts/core';

export const RUNNER_CONTRACT = 4;

export async function checkCoreContract(coreUrl: string, fetcher: typeof fetch = fetch): Promise<void> {
  let meta: { runner_contract_version?: unknown } | null;
  try {
    const response = await fetcher(`${coreUrl}/v1/meta`, { redirect: 'error', signal: AbortSignal.timeout(10_000) });
    if (!response.ok) throw new Error();
    meta = await response.json() as typeof meta;
  } catch {
    throw new Error('core runner contract unavailable from /v1/meta');
  }
  const version = meta?.runner_contract_version;
  if (typeof version !== 'number' || !Number.isSafeInteger(version) || version < RUNNER_CONTRACT) {
    throw new Error(`unsupported core runner contract: /v1/meta must advertise at least version ${RUNNER_CONTRACT}. Upgrade the core first.`);
  }
}

export async function registerRunner(input: {
  coreUrl: string; runnerUrl: string; bootstrapToken: string; version: string;
  vaultIdentityKey?: string;
  runnerName: string; groupId: string; groupName: string;
}, fetcher: typeof fetch = fetch): Promise<RegisterResponse & JoinResponse> {
  await checkCoreContract(input.coreUrl, fetcher);
  async function post<T>(path: string, token: string, body: Record<string, unknown>): Promise<T> {
    const response = await fetcher(`${input.coreUrl}${path}`, {
      method: 'POST', headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
      body: JSON.stringify(body), redirect: 'error', signal: AbortSignal.timeout(10_000),
    });
    if (!response.ok) throw new Error('runner registration request failed');
    return await response.json() as T;
  }
  const installation = await post<RegisterResponse>('/v1/runner-installs/register', input.bootstrapToken, {
    url: input.runnerUrl, version: input.version, contract_version: RUNNER_CONTRACT,
    vault_identity_key: input.vaultIdentityKey,
  });
  const runner = await post<JoinResponse>(`/v1/runner-installs/${encodeURIComponent(installation.install_id)}/runners`,
    installation.join_token, { name: input.runnerName, group_id: input.groupId, group_name: input.groupName, version: input.version });
  return { ...installation, ...runner };
}
