// `mcp-bearer` (design §7.2, §9): a bearer token the gateway adds when it
// calls a remote MCP server (`mcp` delivery). The binding is the server URL
// and the allowed groups; the header is fixed.
import type { CredentialTypeImpl } from './index.ts';
import { canonicalGroups, type Groups, httpsUrl, record } from './common.ts';

export const mcpBearer: CredentialTypeImpl = {
  type: 'mcp-bearer',
  secretFields: ['token'],
  deliveries: ['mcp'],
  binding(config: unknown, groups: Groups) {
    const c = record(config);
    return {
      url: httpsUrl(c.url, 'url'),
      header_name: 'authorization',
      header_format: 'Bearer {}',
      groups: canonicalGroups(groups),
    };
  },
  testRequest(binding) {
    return {
      method: 'POST',
      url: binding.url as string,
      headers: { 'content-type': 'application/json', accept: 'application/json, text/event-stream' },
      body: JSON.stringify({
        jsonrpc: '2.0',
        id: 1,
        method: 'initialize',
        params: { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'runbook-runner-test', version: '1' } },
      }),
    };
  },
};
