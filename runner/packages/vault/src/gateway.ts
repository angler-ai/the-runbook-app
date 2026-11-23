// The vault's client for the gateway's internal routes: material is pushed
// there (and deleted there), never returned to a caller (design §7.5).
import type { ClaimFence, GatewayTestRequest, GatewayTestResponse, MaterialPush, McpDiscoveryRequest } from '@runbook/runner-shared/contracts/internal';
import { VaultError } from './errors.ts';

export interface GatewayOptions {
  baseUrl: string;
  token: string;
  fetch?: typeof fetch;
}

export class GatewayClient {
  private readonly o: GatewayOptions;
  constructor(options: GatewayOptions) {
    this.o = options;
  }

  private async call(method: string, path: string, body?: unknown, timeout = 15_000): Promise<Response> {
    const doFetch = this.o.fetch ?? fetch;
    try {
      return await doFetch(this.o.baseUrl + path, {
        method,
        headers: {
          authorization: `Bearer ${this.o.token}`,
          ...(body === undefined ? {} : { 'content-type': 'application/json' }),
        },
        body: body === undefined ? null : JSON.stringify(body),
        redirect: 'error',
        signal: AbortSignal.timeout(timeout),
      });
    } catch {
      throw new VaultError(502, 'release.gateway_unavailable', 'the gateway could not be reached');
    }
  }

  async epoch(): Promise<string | undefined> {
    const res = await this.call('GET', '/internal/material/epoch', undefined, 2_000);
    await res.body?.cancel();
    // A rolling upgrade can still encounter the old gateway until it is replaced.
    if (res.status === 404) return;
    if (!res.ok) throw new VaultError(502, 'release.gateway_unavailable', 'the gateway could not be reached');
    return res.headers.get('runbook-gateway-epoch') ?? undefined;
  }

  async push(material: MaterialPush): Promise<string | undefined> {
    const res = await this.call('POST', '/internal/material', material);
    await res.arrayBuffer();
    if (!res.ok) throw new VaultError(502, 'release.gateway_unavailable', 'the gateway refused the material');
    return res.headers.get('runbook-gateway-epoch') ?? undefined;
  }

  async endClaim(claim: ClaimFence): Promise<void> {
    const res = await this.call('POST', '/internal/material/end-claim', {
      sub: claim.sub, runner_id: claim.runner_id, claim_id: claim.claim_id, generation: claim.generation,
    });
    if (!res.ok) throw new VaultError(502, 'release.gateway_unavailable', 'the gateway refused the claim fence');
  }

  async revokeCredential(credentialId: string): Promise<void> {
    const res = await this.call('DELETE', `/internal/material/credentials/${encodeURIComponent(credentialId)}`);
    await res.arrayBuffer();
    if (!res.ok) throw new VaultError(502, 'release.gateway_unavailable', 'the gateway refused credential revocation');
  }

  async fenceOAuth(credentialId: string, generation: number): Promise<void> {
    const res = await this.call('POST', `/internal/material/credentials/${encodeURIComponent(credentialId)}/oauth-fence`, { generation });
    await res.arrayBuffer();
    if (!res.ok) throw new VaultError(502, 'release.gateway_unavailable', 'the gateway refused the OAuth fence');
  }

  async lastOAuthUse(credentialId: string, generation: number, version: number): Promise<number | undefined> {
    const res = await this.call('POST', `/internal/material/credentials/${encodeURIComponent(credentialId)}/oauth-use`,
      { generation, token_version: version }, 2_000);
    if (!res.ok) {
      await res.body?.cancel();
      return;
    }
    const value = (await res.json() as { last_success_ms?: unknown } | null)?.last_success_ms;
    if (typeof value === 'number' && Number.isSafeInteger(value)) return value;
  }

  async remove(releaseId: string): Promise<void> {
    const res = await this.call('DELETE', `/internal/material/${encodeURIComponent(releaseId)}`);
    await res.arrayBuffer();
    if (!res.ok && res.status !== 404) throw new VaultError(502, 'release.gateway_unavailable', 'the gateway refused the delete');
  }

  async discover(request: McpDiscoveryRequest): Promise<{ tools: unknown[] }> {
    const response = await this.call('POST', '/internal/mcp/discover', request);
    try {
      if (!response.ok) throw new Error();
      const data = await response.json() as { tools?: unknown };
      if (!Array.isArray(data.tools)) throw new Error();
      return { tools: data.tools };
    } catch {
      throw new VaultError(502, 'mcp.discovery_unavailable', 'MCP tool discovery unavailable');
    }
  }

  /** Run a connection test. Only the outcome and status are read; anything else in the answer is ignored. */
  async test(request: GatewayTestRequest): Promise<GatewayTestResponse> {
    const res = await this.call('POST', '/internal/test', request);
    const text = await res.text();
    if (!res.ok) throw new VaultError(502, 'test.gateway_unavailable', 'the gateway could not run the test');
    let parsed: { outcome?: unknown; status?: unknown };
    try {
      parsed = JSON.parse(text) as typeof parsed;
    } catch {
      throw new VaultError(502, 'test.gateway_unavailable', 'the gateway answered with something else');
    }
    if (parsed.outcome === 'unreachable' || parsed.outcome === 'tls') return { outcome: parsed.outcome };
    if (parsed.outcome === 'response' && Number.isInteger(parsed.status)) return { outcome: 'response', status: parsed.status as number };
    throw new VaultError(502, 'test.gateway_unavailable', 'the gateway answered with something else');
  }
}
