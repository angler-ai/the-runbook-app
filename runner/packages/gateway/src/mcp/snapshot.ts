import { isDeepStrictEqual } from 'node:util';
import type { Tool } from '@modelcontextprotocol/sdk/types.js';
import { HttpError } from '@runbook/runner-shared/http';

export interface PinnedTool {
  tool: Tool;
  label?: 'read' | 'write' | 'destructive';
}

interface Upstream {
  list(): Promise<Tool[]>;
  call(name: string, args: Record<string, unknown>, idempotencyKey?: string): Promise<unknown>;
}

function unavailable(): HttpError {
  return new HttpError(403, 'mcp.tool_unavailable', 'tool unavailable');
}

export function upstreamHeaders(url: string, material?: { url: unknown; token?: string }): Record<string, string> {
  if (!material) return {};
  if (material.url !== url || !material.token) throw unavailable();
  return { authorization: `Bearer ${material.token}` };
}

export class StepTools {
  private readonly pinned: readonly PinnedTool[];
  private readonly upstream: Upstream;
  private readonly destructive: boolean;

  constructor(pinned: readonly PinnedTool[], upstream: Upstream, destructive: boolean) {
    this.pinned = pinned;
    this.upstream = upstream;
    this.destructive = destructive;
  }

  private allowed(entry: PinnedTool): boolean {
    return this.destructive || entry.label === 'read' || entry.label === 'write';
  }

  private matches(expected: Tool, current: Tool[]): boolean {
    const actual = current.find(({ name }) => name === expected.name);
    return !!actual && actual.description === expected.description &&
      isDeepStrictEqual(actual.inputSchema, expected.inputSchema) &&
      isDeepStrictEqual(actual.outputSchema, expected.outputSchema);
  }

  async list(): Promise<Tool[]> {
    const current = await this.upstream.list();
    return this.pinned.filter((entry) => this.allowed(entry) && this.matches(entry.tool, current))
      .map(({ tool }) => tool);
  }

  async call(name: string, args: Record<string, unknown>, idempotencyKey?: string): Promise<unknown> {
    const entry = this.pinned.find(({ tool }) => tool.name === name);
    if (!entry || !this.allowed(entry) || !this.matches(entry.tool, await this.upstream.list())) throw unavailable();
    return this.upstream.call(name, args, idempotencyKey);
  }
}
