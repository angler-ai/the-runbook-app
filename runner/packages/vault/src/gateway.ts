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

  private async call(method: string, path: string, body?: unknown): Promise<Response> {
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
        signal: AbortSignal.timeout(15_000),
      });
    } catch {
      throw new VaultError(502, 'release.gateway_unavailable', 'the gateway could not be reached');
    }
  }

  async push(material: MaterialPush): Promise<void> {
    const res = await this.call('POST', '/internal/material', material);
    await res.arrayBuffer();
    if (!res.ok) throw new VaultError(502, 'release.gateway_unavailable', 'the gateway refused the material');
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
