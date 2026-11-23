import { fetchWithTrace, identifier, inSpan, setLabels } from '@runbook/runner-shared/telemetry';
import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { WebStandardStreamableHTTPServerTransport } from '@modelcontextprotocol/sdk/server/webStandardStreamableHttp.js';
import { CallToolRequestSchema, ListToolsRequestSchema, type CallToolResult } from '@modelcontextprotocol/sdk/types.js';
import type { Label } from '@runbook/runner-shared/contracts/core';
import type { Config } from '@runbook/runner-shared/config';
import { CAPABILITY_HEADER, type ToolCallRecord, type McpDiscoveryRequest } from '@runbook/runner-shared/contracts/internal';
import { HttpError, type Route } from '@runbook/runner-shared/http';
import { redactValue, type Redaction } from '@runbook/runner-shared/redact';
import type { CapabilityVerifier } from '../capability.ts';
import { materialValues, type MaterialStore } from '../material.ts';
import { StepTools, upstreamHeaders, type PinnedTool } from './snapshot.ts';
import { connectUpstream, UpstreamAccessBlocked } from './upstream.ts';

interface ServerSnapshot {
  id: string;
  url: string;
  transport: 'streamable-http' | 'sse';
  credential_id?: string;
  oauth?: boolean;
  tools: PinnedTool[];
}

interface StepSnapshot {
  destructive: boolean;
  credentialLabels: Record<string, Label>;
  servers: ServerSnapshot[];
}

export function mcpRoute(config: Config, verifier: Pick<CapabilityVerifier, 'verifyCapability'>, store: MaterialStore, redaction: Redaction): Route {
  return {
    method: 'ALL', path: '/mcp/{step_run_id}', callers: ['worker'],
    async handle({ params, body, request, runnerId }): ReturnType<Route['handle']> {
      const capability = request?.headers[CAPABILITY_HEADER.toLowerCase()];
      if (typeof capability !== 'string') throw new HttpError(401, 'capability.invalid', 'the step capability is invalid');
      const token = capability;
      const claims = await verifier.verifyCapability(token);
      const authenticatedRunner = runnerId ?? (config.runnerIds?.length === 1 ? config.runnerIds[0] : undefined);
      if (!authenticatedRunner || claims.runner_id !== authenticatedRunner) {
        throw new HttpError(403, 'mcp.runner_denied', 'worker is not authorized for this runner');
      }
      if (claims.sub !== params.step_run_id || !store.active(claims)) throw new HttpError(403, 'mcp.step_denied', 'step unavailable');

      if (Array.isArray(body)) throw new HttpError(400, 'mcp.batch_unsupported', 'send one MCP request');
      const server = new Server({ name: 'runbook-gateway', version: '1' }, { capabilities: { tools: {} } });
      const transport = new WebStandardStreamableHTTPServerTransport({ enableJsonResponse: true });
      let record: ToolCallRecord | undefined;
      async function snapshot(): Promise<StepSnapshot> {
        const response = await fetchWithTrace(`${config.urls.api}/internal/mcp/snapshot`, {
          method: 'POST',
          headers: {
            authorization: `Bearer ${config.outboundTokens.api}`,
            'content-type': 'application/json',
          },
          body: JSON.stringify({ capability }),
          signal: AbortSignal.timeout(10_000),
          redirect: 'error',
        });
        if (!response.ok) throw new Error('snapshot unavailable');
        const policy = await response.json() as StepSnapshot;
        if (claims.reach !== 'read') return policy;
        return {
          destructive: false,
          credentialLabels: Object.fromEntries(Object.entries(policy.credentialLabels)
            .filter(([id, label]) => label === 'read' && claims.connections?.includes(id))),
          servers: policy.servers.flatMap((entry) => {
            if (entry.credential_id && (!claims.connections?.includes(entry.credential_id) ||
                policy.credentialLabels[entry.credential_id] !== 'read')) return [];
            const tools = entry.tools.filter(({ label }) => label === 'read');
            return tools.length ? [{ ...entry, tools }] : [];
          }),
        };
      }

      async function withTools<T>(entry: ServerSnapshot, policy: StepSnapshot, action: (tools: StepTools, secrets: string[]) => Promise<T>) {
        if (entry.credential_id) {
          const label = Object.hasOwn(policy.credentialLabels ?? {}, entry.credential_id)
            ? policy.credentialLabels[entry.credential_id] : undefined;
          if (label !== 'read' && label !== 'write' && !(label === 'destructive' && policy.destructive === true)) {
            throw new Error('MCP credential unavailable');
          }
        }
        const secrets = new Set(store.forStep(claims).flatMap(({ material }) => materialValues(material)));
        const observedSecrets: string[] = [...secrets];
        const upstream = await connectUpstream(entry.url, entry.transport, async () => {
          const current = await verifier.verifyCapability(token);
          if (current.runner_id !== authenticatedRunner || !store.active(current)) throw new Error('MCP claim unavailable');
          const release = entry.credential_id ? store.forStep(current).find((candidate) =>
            candidate.delivery === 'mcp' && candidate.credential_id === entry.credential_id) : undefined;
          if (entry.credential_id && !release) {
            if (entry.oauth) throw new UpstreamAccessBlocked({ kind: 'missing_access' });
            throw new Error('MCP release unavailable');
          }
          const oauth = release?.material.mcp_oauth;
          const deadline = AbortSignal.timeout(Math.max(0, current.exp * 1000 - Date.now()));
          for (const secret of materialValues(release?.material)) {
            if (!secrets.has(secret)) observedSecrets.push(secret);
            secrets.add(secret);
          }
          return {
            headers: upstreamHeaders(entry.url, release && { url: release.binding.url, token: release.material.mcp_oauth?.access_token ?? release.material.fields.token }),
            ...(oauth && release && {
              oauth: { release_id: release.release_id, token_version: oauth.token_version },
              accepted: () => store.noteOAuthUse(release),
            }),
            signal: release ? AbortSignal.any([deadline, store.signal(release)]) : deadline,
          };
        }, request?.signal);
        try {
          return await action(new StepTools(entry.tools, upstream, policy.destructive === true), observedSecrets);
        } finally {
          await upstream.close();
        }
      }

      server.setRequestHandler(ListToolsRequestSchema, async () => {
        try {
          const pinned = await snapshot();
          const lists = await Promise.all(pinned.servers.map((entry) =>
            withTools(entry, pinned, async (tools) => (await tools.list()).map((tool) => ({
              ...tool, name: `${entry.id}__${tool.name}`,
              _meta: { ...tool._meta, 'runbook/label': entry.tools.find(({ tool: pinned }) => pinned.name === tool.name)?.label ?? 'destructive' },
            })))));
          return { tools: lists.flat() };
        } catch (error) {
          if (error instanceof UpstreamAccessBlocked) throw error;
          throw new Error('tool list unavailable');
        }
      });
      server.setRequestHandler(CallToolRequestSchema, ({ params: call }) => inSpan('mcp.tool.call', async (span) => {
        const started = Date.now();
        try {
          const pinned = await snapshot();
          const matches = pinned.servers.flatMap((entry) => entry.tools
            .filter(({ tool }) => `${entry.id}__${tool.name}` === call.name)
            .map(({ tool }) => ({ entry, name: tool.name })));
          const match = matches[0];
          if (!match || matches.length !== 1) throw new Error('tool unavailable');
          const server = identifier(match.entry.id);
          const tool = identifier(match.name);
          if (server) span.setAttribute('mcp.server.name', server);
          if (tool) span.setAttribute('mcp.tool.name', tool);
          setLabels({ project: claims.project_id, step_run: claims.sub });
          const args = call.arguments ?? {};
          const liveSecrets = store.forStep(claims).flatMap(({ material }) => materialValues(material));
          const redacted = redactValue(args, (value) => redaction.redact(value, liveSecrets)) as Record<string, unknown>;
          record = {
            capability, server_id: match.entry.id, tool: match.name, result_bytes: 0, duration_ms: 0,
            args_bytes: Buffer.byteLength(JSON.stringify(args)),
            args_redacted: Buffer.byteLength(JSON.stringify(redacted)) <= 32 * 1024 ? redacted : { truncated: true },
          };
          const result = await withTools(match.entry, pinned, async (tools, secrets) => {
            const key = call._meta?.['runbook/idempotency-key'];
            const idempotency = typeof key === 'string' && key.length <= 512 && key.startsWith(`${claims.sub}:`) ? key : undefined;
            const raw = await tools.call(match.name, args, idempotency);
            // Redact decoded values before serialization so JSON escaping cannot hide secrets.
            return redactValue(raw, (value) => redaction.redact(value, secrets)) as CallToolResult;
          });
          record.result_bytes = Buffer.byteLength(JSON.stringify(result));
          if (result.isError) record.error = 'tool_failed';
          return result;
        } catch (error) {
          if (record) record.error = 'tool_unavailable';
          if (error instanceof UpstreamAccessBlocked) throw error;
          return { content: [{ type: 'text', text: 'tool unavailable' }], isError: true };
        } finally {
          if (record) record.duration_ms = Date.now() - started;
        }
      }));

      const method = request?.method ?? 'GET';
      // Stateless JSON mode cannot open a GET event stream or delete a session.
      if (method !== 'POST') return { status: 405, headers: { allow: 'POST' } };
      const headers = new Headers();
      for (const name of ['accept', 'content-type', 'mcp-protocol-version']) {
        const value = request?.headers[name];
        if (typeof value === 'string') headers.set(name, value);
      }
      await server.connect(transport);
      try {
        const reply = await transport.handleRequest(new Request('https://gateway.invalid/mcp', { method, headers }), { parsedBody: body });
        const result = await reply.text();
        if (record) {
          try {
            const written = await fetchWithTrace(`${config.urls.api}/internal/mcp/tool-calls`, {
              method: 'POST',
              headers: { authorization: `Bearer ${config.outboundTokens.api}`, 'content-type': 'application/json' },
              body: JSON.stringify(record), redirect: 'error', signal: AbortSignal.timeout(10_000),
            });
            if (!written.ok) throw new Error();
          } catch {
            throw new HttpError(503, 'mcp.record_unavailable', 'tool call record unavailable');
          }
        }
        return {
          status: reply.status,
          ...(result && { body: JSON.parse(result) as unknown }),
        };
      } finally {
        await server.close();
      }
    },
  };
}

export function discoveryRoute(store: MaterialStore, redaction: Redaction): Route {
  return {
    method: 'POST', path: '/internal/mcp/discover', callers: ['vault'],
    async handle({ body }) {
      const input = body as Partial<McpDiscoveryRequest> | undefined;
      if (!input || typeof input.url !== 'string' ||
          (input.transport !== 'streamable-http' && input.transport !== 'sse') ||
          (input.release_id !== undefined && typeof input.release_id !== 'string')) {
        throw new HttpError(400, 'mcp.invalid_discovery', 'invalid MCP discovery request');
      }
      try {
        const secrets: string[] = [];
        const upstream = await connectUpstream(input.url, input.transport, async () => {
          const release = input.release_id ? store.get(input.release_id) : undefined;
          if (input.release_id && (!release || release.delivery !== 'mcp' || release.step_run_id !== `test:${release.release_id}`)) {
            throw new Error('MCP release unavailable');
          }
          if (release) secrets.push(...materialValues(release.material));
          return {
            headers: upstreamHeaders(input.url!, release && { url: release.binding.url, token: release.material.mcp_oauth?.access_token ?? release.material.fields.token }),
            ...(release && { signal: store.signal(release) }),
          };
        });
        try {
          const tools = (await upstream.list()).map(({ name, description, inputSchema, outputSchema }) =>
            ({ name, inputSchema, ...(description !== undefined && { description }), ...(outputSchema !== undefined && { outputSchema }) }));
          const safe = redactValue(tools, (value) => redaction.redact(value, secrets));
          if (JSON.stringify(safe) !== JSON.stringify(tools)) throw new Error('tool schema contains sensitive data');
          return { body: { tools } };
        } finally {
          await upstream.close();
        }
      } catch {
        throw new HttpError(502, 'mcp.discovery_unavailable', 'MCP tool discovery unavailable');
      }
    },
  };
}
