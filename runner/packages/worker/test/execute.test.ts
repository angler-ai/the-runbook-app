import { sandboxIdForClaim } from '@runbook/runner-shared/capability';
import { loadConfig } from '@runbook/runner-shared/config';
import type { GrantClaims } from '@runbook/runner-shared/contracts/core';
import { loadRedaction } from '@runbook/runner-shared/redact';
import { expect, it, vi } from 'vitest';
import { workerClients } from '../src/clients.ts';
import { claimExecutor } from '../src/execute.ts';

it.each([['v1', 'safe'], ['v2', 'safe'], ['v2', 'untyped'], ['v2', 'typed'], ['v2', 'setup-ok'], ['v2', 'setup-error'], ['v2', 'setup-timeout'], ['v2', 'compute-budget']])(
  'uses the profile key and protects reported output in %s (%s)', async (version, resultKind) => {
  const bearer = 't'.repeat(32);
  const config = loadConfig('worker', {
    RUNNER_ROLE: 'worker', RUNNER_ID: 'r', RUNNER_GROUP_ID: 'g', CORE_URL: 'https://core.fixture',
    CORE_CLAIM_TOKEN: bearer, RUNNER_DATABASE_URL: 'postgres://localhost/runbook',
    API_URL: 'https://api.fixture', VAULT_URL: 'https://vault.fixture', GATEWAY_URL: 'https://gateway.fixture',
    SANDBOX_CTL_URL: 'https://sandbox.fixture', RUNNER_TOKEN_API_WORKER: bearer, RUNNER_TOKEN_WORKER_API: bearer,
    RUNNER_TOKEN_WORKER_VAULT: bearer, RUNNER_TOKEN_WORKER_GATEWAY: bearer, RUNNER_TOKEN_WORKER_SANDBOXCTL: bearer,
  });
  const claims = { sub: 'step', runner_id: 'r', group_id: 'g', project_id: 'p', agent_id: 'a',
    claim_id: 'claim', generation: 1, connections: null, step_deadline_ms: null } as GrantClaims;
  const sandbox = sandboxIdForClaim(claims);
  const hasSetup = resultKind.startsWith('setup-');
  const setupFail = hasSetup && resultKind !== 'setup-ok';
  const image = `registry.example/runner@sha256:${'a'.repeat(64)}`;
  const requests: unknown[] = [];
  const entries: string[] = [];
  const contextCalls: URL[] = [];
  const clients = workerClients(config, async (input, init) => {
    const request = new Request(input, init);
    switch (new URL(request.url).pathname) {
      case '/v1/step-runs/step':
        return Response.json({ template: { instructions: 'Report the "result".\nVerify the evidence.' }, checklist: [],
          ...(resultKind === 'typed' && { output_schema: { type: 'object', properties: { ok: { type: 'string' } } } }),
        });
      case '/v1/step-runs/step/context/run':
        return Response.json({ id: 'run-1', title: 'Tuesday deployment', project: 'ops', runbook_slug: 'deploy',
          step: { id: 'verify', title: 'Verify deployment' } });
      case '/v1/step-runs/step/logs':
        return Response.json(1);
      case '/v1/step-runs/step/context/memory': {
        expect(request.headers.get('authorization')).toBe(`Bearer ${bearer}`);
        contextCalls.push(new URL(request.url));
        return Response.json({ items: [{ body_markdown: 'released-value fixture-runbook tok_live_example' }] });
      }
      case '/internal/worker/step':
        return Response.json({ profile: 'fast', max_output_tokens: 100, credentials: [
          { credential_id: 'model-key', delivery: 'model' },
        ], instructions: '', limits: { max_tokens: 100, timeout_s: 60 }, size: 'small',
          ...(hasSetup && { environment: { image, has_setup: true } }) });
      case '/internal/release':
        requests.push(await request.json());
        return Response.json({ release_id: 'key' });
      case '/internal/sandboxes':
        expect(await request.json()).toMatchObject({ image: hasSetup ? image : 'image' });
        if (resultKind === 'compute-budget') return Response.json({ level: 'environment', budget: {
          scope: 'environment', key: 'env-1', period: 'month', cap: 2, detail: 'environment Example: monthly budget $2 reached',
        } }, { status: 402 });
        return Response.json({ sandbox_id: sandbox });
      case `/internal/sandboxes/${sandbox}/setup`:
        expect(request.headers.get('x-runbook-capability')).toBe('capability');
        return Response.json({ exit_code: setupFail ? 1 : 0, timed_out: resultKind === 'setup-timeout',
          stdout: 'setup says released-value', stderr: '' });
      case '/internal/claims/transcript': {
        const { role, value } = await request.json() as { role: string; value: unknown };
        entries.push(JSON.stringify({ role, value }));
        return new Response(null, { status: 204 });
      }
      case '/internal/claims/redact': {
        const { value } = await request.json() as { value: unknown };
        return Response.json({ value: JSON.parse(JSON.stringify(value).replaceAll('released-value', '[REDACTED]')) });
      }
      default:
        throw new Error('unexpected request');
    }
  });
  const messages: string[] = [];
  const modelResult = resultKind === 'safe' || resultKind === 'setup-ok' ? { outputs: { ok: true }, summary: 'finished' }
    : { outputs: { ok: 'released-value' }, summary: 'summary contains released-value' };
  const model = vi.fn<typeof fetch>(async (_input, init) => {
    messages.push(String(init?.body));
    if (version === 'v2') {
      const id = `call-${messages.length}`;
      const parts = [
        { type: 'tool-call', toolCallId: id, toolName: messages.length === 1 ? 'search_memory' : 'submit_result',
          input: JSON.stringify(messages.length === 1 ? { q: 'baseline & incident' } :
            { ...modelResult, evidence: [], unverified: [] }) },
        { type: 'finish', finishReason: { raw: 'tool_use', unified: 'tool-calls' },
          usage: { inputTokens: { total: 2 }, outputTokens: { total: 3 } } },
      ];
      return new Response(parts.map((part) => `event: part\ndata: ${JSON.stringify(part)}\n\n`).join(''),
        { headers: { 'content-type': 'text/event-stream' } });
    }
    return Response.json({
      text: JSON.stringify(messages.length === 1
        ? { tool: 'search_memory', arguments: { q: 'baseline & incident' } }
        : { outputs: { ok: true }, summary: 'finished' }),
      usage: { input_tokens: 2, output_tokens: 3 },
    });
  });
  vi.stubGlobal('fetch', model);
  vi.stubEnv('RUNNER_HARNESS', version);
  try {
    const execute = claimExecutor(config, clients, { verifyCapability: async () => claims }, 'image',
      { redaction: await loadRedaction(), installId: 'install' });
    const result = await execute({ stepRunId: 'step', claim: { id: 'claim', generation: 1 },
      signal: new AbortController().signal, capability: () => 'capability', registerSandbox: () => undefined,
      sandboxStarted: () => undefined });
    if (resultKind === 'compute-budget') {
      expect(result).toMatchObject({ status: 'failed', summary: 'environment Example: monthly budget $2 reached' });
      expect(model).not.toHaveBeenCalled();
      return;
    }
    if (hasSetup) expect(entries[0]).toContain('setup says [REDACTED]');
    if (setupFail) {
      expect(result).toMatchObject({ status: 'failed', outputs: {}, summary: 'environment_setup_failed' });
      expect(model).not.toHaveBeenCalled();
      return;
    }
    expect(result).toMatchObject(resultKind === 'safe' || resultKind === 'setup-ok'
      ? { status: 'succeeded', outputs: { ok: true }, summary: 'finished' }
      : resultKind === 'typed'
        ? { status: 'failed', outputs: {}, summary: 'output_redacted' }
        : { status: 'succeeded', outputs: { ok: '[REDACTED]' }, summary: 'summary contains [REDACTED]' });
    expect(requests).toEqual([{ capability: 'capability', credential_id: 'model-key', delivery: 'model' }]);
    expect(model).toHaveBeenCalledTimes(2);
    expect(Object.fromEntries(contextCalls[0]!.searchParams)).toEqual({
      claim_id: 'claim', generation: '1', q: 'baseline & incident',
    });
    if (version === 'v1') expect(messages[1]).toContain('untrusted data');
    else {
      const { request: { prompt } } = JSON.parse(messages[1]!);
      expect(prompt.find(({ role }: { role: string }) => role === 'user').content[0].text)
        .toContain('Report the "result".\nVerify the evidence.\n\nStep data:');
      expect(JSON.parse(JSON.stringify(prompt.at(-1)).replace(/ \[[a-f0-9]{32}\] ---/g, ' ---'))).toMatchObject({ role: 'tool', content: [{
        output: { type: 'text', value: '[call 1]\n--- Untrusted data from Runbook ---\nitems:\n  - body_markdown: [REDACTED] fixture-runbook tok_live_example\n--- End untrusted data ---' },
      }] });
    }
    expect(messages[1]).toContain('fixture-runbook');
    expect(messages[1]).not.toContain('released-value');
    expect(entries.some((entry) => entry.includes('"role":"tool"') && entry.includes('fixture-runbook'))).toBe(true);
  } finally {
    vi.unstubAllGlobals();
    vi.unstubAllEnvs();
  }
});
