import type { AnthropicProviderOptions } from '@ai-sdk/anthropic';
import { expect, it, vi } from 'vitest';
import type { LanguageModelV4StreamPart, LanguageModelV4Usage } from '@ai-sdk/provider';
import { gatewayModel } from '@runbook/runner-shared/gateway-model-provider';
import type { ModelProfile } from '../src/model/endpoint.ts';
import { reservationInput, streamModel, usageCost, validateCall } from '../src/model/stream.ts';

const profile: ModelProfile = {
  credentialId: 'fixture', provider: 'anthropic', model: 'claude-sonnet-5-5', endpoint: 'https://example.test',
  maxOutputTokens: 32000, inputUsdPerMillion: 2, outputUsdPerMillion: 10,
  cacheReadUsdPerMillion: 0.2, cacheWriteUsdPerMillion: 4,
};
const usage: LanguageModelV4Usage = {
  inputTokens: { total: 15100, noCache: 100, cacheRead: 15000, cacheWrite: 0 },
  outputTokens: { total: 70, reasoning: 50, text: 20 },
};
const finish = { type: 'finish', finishReason: { unified: 'tool-calls', raw: 'tool_use' }, usage } as const;
const events: LanguageModelV4StreamPart[] = [
  { type: 'reasoning-start', id: 'r' }, { type: 'reasoning-delta', id: 'r', delta: 'think' },
  { type: 'reasoning-end', id: 'r', providerMetadata: { anthropic: { signature: 'signed' } } },
  { type: 'tool-call', toolCallId: 'original', toolName: 'view', input: '{"path":"fixture"}' }, finish,
];
const defaults = () => ({
  signal: new AbortController().signal, availableMs: 5000, secrets: ['fixture-secret'],
  reserve: vi.fn(async () => ({ reservation_id: 'fixture', amount_usd: 0.1 })),
  settle: vi.fn(async () => undefined), finished: vi.fn(),
});

it('round trips signed reasoning, tool IDs, activity and raw finish usage', async () => {
  const deps = defaults();
  const request = { prompt: [{ role: 'assistant' as const,
    content: [{ type: 'reasoning' as const, text: 'thinking', providerOptions: { anthropic: { signature: 'signed' } } }] }],
  maxOutputTokens: 32000 };
  const model = gatewayModel('sonnet', {
    gatewayUrl: 'http://fixture', streaming: true, capability: () => 'cap', processToken: 'process', maxOutputTokens: 32000,
    async fetch(_url, init) {
      const input = JSON.parse(String(init?.body));
      expect(input.request).toEqual(request);
      const result = await streamModel(input, profile, { ...deps, async invoke(call, tick) {
        expect(call.maxOutputTokens).toBeLessThan(32000);
        tick();
        return { stream: new ReadableStream({ start(controller) {
          for (const event of events) controller.enqueue(event);
          controller.close();
        } }) };
      } });
      return new Response(result.body, { headers: result.headers });
    },
  });
  const result = await model.doStream(request);
  const actual: LanguageModelV4StreamPart[] = [];
  for await (const part of result.stream) actual.push(part);
  expect(actual).toEqual([{ type: 'raw', rawValue: { runnerActivity: true } }, ...events]);
  expect(deps.settle).toHaveBeenCalledWith(expect.objectContaining({ actual_usd: usageCost(profile, usage) }));
});

it('rejects worker-selected speed, arbitrary betas and server tools', () => {
  const request = { prompt: [{ role: 'system' as const, content: 'fixture' }], maxOutputTokens: 100 };
  for (const anthropic of [{ speed: 'fast' }, { anthropicBeta: ['unpriced'] }, { fallbacks: 'default' }]) {
    expect(() => validateCall(profile, { ...request, providerOptions: { anthropic } })).toThrow('not allowed');
  }
  expect(() => validateCall(profile, { ...request, tools: [{ type: 'provider', id: 'anthropic.fake', name: 'fake', args: {} }] }))
    .toThrow('not allowed');
});

it('settles cache discounts while reserving for full cache creation across turns', async () => {
  let remaining = 1;
  const prompt = [{ role: 'system' as const, content: 'fixture '.repeat(5000) }];
  for (let turn = 0; turn < 25; turn++) {
    const request = { prompt: [...prompt], maxOutputTokens: 32000 };
    expect(reservationInput(profile, request)).toBeGreaterThanOrEqual(40_000 * 4 / 1_000_000);
    const result = await streamModel({ profile: profile.model, capability: 'cap', request }, profile, {
      ...defaults(),
      async reserve(reservation) {
        expect(reservation.minimum_usd).toBeLessThan(remaining);
        return { reservation_id: 'fixture', amount_usd: Math.min(remaining, reservation.amount_usd) };
      },
      async settle(settlement) { remaining -= settlement.actual_usd; },
      async invoke() {
        return { stream: new ReadableStream({ start(controller) { controller.enqueue(finish); controller.close(); } }) };
      },
    });
    expect(await new Response(result.body).text()).toContain('tool_use');
    prompt.push({ role: 'system', content: 'another turn' });
  }
  expect(remaining).toBeGreaterThan(0);
});

it('reserves for dense tool definitions even when the system prompt repeats', () => {
  const prompt = [{ role: 'system' as const, content: 'fixture' }];
  const request = { prompt, tools: [{ type: 'function' as const, name: 'lookup',
    description: '界'.repeat(20_000), inputSchema: { type: 'object' as const } }] };
  expect(reservationInput(profile, request)).toBeGreaterThanOrEqual(60_000 * 4 / 1_000_000);
});

it.each(['input-only', 'unknown', 'pre-dispatch'])('settles an interrupted %s call conservatively', async (kind) => {
  const deps = defaults();
  const request = { prompt: [{ role: 'system' as const, content: 'fixture' }], maxOutputTokens: 100 };
  const result = await streamModel({ profile: profile.model, capability: 'cap', request, attemptLimit: 1 }, profile, {
    ...deps,
    async invoke(_call, activity, observe, dispatched) {
      if (kind === 'pre-dispatch') throw new Error('failed before sending');
      dispatched();
      if (kind === 'input-only') {
        activity();
        observe(profile.model, { inputTokens: { total: 4000, noCache: 4000, cacheRead: 0, cacheWrite: 0 },
          outputTokens: { total: undefined, text: undefined, reasoning: undefined } });
      }
      return { stream: new ReadableStream({ start(controller) { controller.close(); } }) };
    },
  });
  await new Response(result.body).text();
  expect(deps.settle).toHaveBeenCalledWith(expect.objectContaining({
    actual_usd: kind === 'pre-dispatch' ? 0 : 0.1,
    ...(kind !== 'pre-dispatch' && { usage_pending: true }),
    usage: kind === 'input-only' ? expect.objectContaining({ input_tokens: 4000 }) : {},
  }));
});

it('keeps wire-observed input usage when the SDK finish only reports output', async () => {
  const deps = defaults();
  const request = { prompt: [{ role: 'system' as const, content: 'fixture' }], maxOutputTokens: 100 };
  const result = await streamModel({ profile: profile.model, capability: 'cap', request }, profile, {
    ...deps,
    async invoke(_call, _activity, observe) {
      observe(profile.model, { inputTokens: { total: 4000, noCache: 4000, cacheRead: 0, cacheWrite: 0 },
        outputTokens: { total: undefined, text: undefined, reasoning: undefined } });
      return { stream: new ReadableStream({ start(controller) {
        controller.enqueue({ ...finish, usage: {
          inputTokens: { total: undefined, noCache: undefined, cacheRead: undefined, cacheWrite: undefined },
          outputTokens: { total: 10, text: 10, reasoning: 0 },
        } });
        controller.close();
      } }) };
    },
  });
  await new Response(result.body).text();
  expect(deps.settle).toHaveBeenCalledWith(expect.objectContaining({
    actual_usd: 0.0081, usage: expect.objectContaining({ input_tokens: 4000, output_tokens: 10 }),
  }));
});

it('retries pre-byte overload and withholds a credential split across chunks', async () => {
  const deps = defaults();
  const invoke = vi.fn().mockRejectedValueOnce({ statusCode: 429, responseHeaders: { 'retry-after': '0.01' } })
    .mockResolvedValueOnce({ stream: new ReadableStream({ start(controller) {
      controller.enqueue({ type: 'text-delta', id: 'text', delta: 'fixture-' });
      controller.enqueue({ type: 'text-delta', id: 'text', delta: 'secret' });
      controller.close();
    } }) });
  const result = await streamModel({ profile: 'sonnet', capability: 'cap',
    request: { prompt: [{ role: 'system', content: 'fixture' }], maxOutputTokens: 100 } }, profile, { ...deps, invoke });
  const wire = await new Response(result.body).text();
  expect(wire).toContain('secret_in_model_output');
  expect(wire).not.toContain('fixture-');
  expect(invoke).toHaveBeenCalledTimes(2);
  expect(deps.settle).toHaveBeenCalledWith(expect.objectContaining({ actual_usd: 0.1 }));
});

it('relays real Anthropic SDK parts from fixture wire events, including hidden pings and raw usage', async () => {
  const upstreamEvents = [
    { type: 'message_start', message: { id: 'msg_fixture', type: 'message', role: 'assistant', model: profile.model,
      content: [], stop_reason: null, stop_sequence: null,
      usage: { input_tokens: 100, cache_read_input_tokens: 15000, cache_creation_input_tokens: 50, output_tokens: 0 } } },
    { type: 'ping' },
    { type: 'content_block_start', index: 0, content_block: { type: 'thinking', thinking: '' } },
    { type: 'content_block_delta', index: 0, delta: { type: 'thinking_delta', thinking: 'fixture thinking' } },
    { type: 'content_block_delta', index: 0, delta: { type: 'signature_delta', signature: 'signed-fixture' } },
    { type: 'content_block_stop', index: 0 },
    { type: 'content_block_start', index: 1, content_block: { type: 'tool_use', id: 'toolu_fixture', name: 'view', input: {} } },
    { type: 'content_block_delta', index: 1, delta: { type: 'input_json_delta', partial_json: '{"path":"fixture"}' } },
    { type: 'content_block_stop', index: 1 },
    { type: 'message_delta', delta: { stop_reason: 'tool_use', stop_sequence: null }, usage: { output_tokens: 70 } },
    { type: 'message_stop' },
  ];
  const deps = defaults();
  const { providerModel } = await import('../src/model/providers.ts');
  const { observeProviderFetch } = await import('../src/model/stream-fetch.ts');
  const result = await streamModel({ capability: 'cap', profile: profile.model, request: {
    prompt: [{ role: 'system', content: 'fixture', providerOptions: { anthropic: { cacheControl: { type: 'ephemeral', ttl: '1h' } } } },
      { role: 'user', content: [{ type: 'text', text: 'Read a fixture.' }] }],
    maxOutputTokens: 1024,
    providerOptions: { anthropic: { thinking: { type: 'adaptive', display: 'updates' } } },
    tools: [{ type: 'function', name: 'view', inputSchema: { type: 'object', properties: { path: { type: 'string' } },
      required: ['path'], additionalProperties: false } }],
  } }, profile, {
    ...deps,
    invoke: (call, tick, observe) => providerModel(profile, { api_key: 'fixture-secret' }, () =>
      observeProviderFetch(async (_input, init) => {
        expect(JSON.parse(String(init?.body))).toMatchObject({
          model: profile.model, thinking: { type: 'adaptive', display: 'updates' },
          system: [{ cache_control: { type: 'ephemeral', ttl: '1h' } }],
        });
        return new Response(new ReadableStream({ start(controller) {
          for (const event of upstreamEvents) controller.enqueue(new TextEncoder().encode(`data: ${JSON.stringify(event)}\n\n`));
          controller.close();
        } }), { headers: { 'content-type': 'text/event-stream' } });
      }, tick, observe)).doStream(call),
  });
  const wire = await new Response(result.body).text();
  expect(wire).toContain('event: activity');
  expect(wire).toContain('signed-fixture');
  expect(wire).toContain('toolu_fixture');
  expect(wire).toContain('"raw":"tool_use"');
  expect(wire).toContain(profile.model);
  expect(wire).not.toContain('fixture-secret');
  expect(deps.settle).toHaveBeenCalledWith(expect.objectContaining({
    usage: { input_tokens: 15150, cached_tokens: 15000, cache_write_tokens: 50, output_tokens: 70 },
  }));
});

it('settles a complete fallback on the served model and keeps a broken fallback pending', async () => {
  const alternate = { ...profile, model: 'claude-fallback', inputUsdPerMillion: 8,
    cacheReadUsdPerMillion: 0.8, cacheWriteUsdPerMillion: 16, outputUsdPerMillion: 40 };
  for (const complete of [true, false]) {
    const deps = defaults();
    const result = await streamModel({ capability: 'cap', profile: profile.model, request: {
      prompt: [{ role: 'system', content: 'fixture' }], maxOutputTokens: 1000,
      providerOptions: { anthropic: { fallbacks: [{ model: alternate.model }] } },
    } }, { ...profile, servedPrices: [alternate] }, {
      ...deps,
      async invoke(_call, tick, observe) {
        tick();
        observe(alternate.model, usage);
        return { stream: new ReadableStream({ start(controller) {
          controller.enqueue({ type: 'response-metadata', modelId: alternate.model });
          controller.enqueue({ type: 'text-delta', id: 'text', delta: 'fixture output' });
          controller.enqueue(complete ? finish : { type: 'error', error: 'reset' });
          controller.close();
        } }) };
      },
    });
    expect(await new Response(result.body).text()).toContain(complete ? 'tool_use' : 'model.call_failed');
    expect(deps.settle).toHaveBeenCalledWith(expect.objectContaining(complete
      ? { actual_usd: usageCost(alternate, usage) }
      : { actual_usd: 0.1, usage_pending: true }));
  }
});

it('allows caller, citations, toolset metadata and compatible thought signatures only on parts', () => {
  const request = { prompt: [{ role: 'assistant' as const, content: [
    { type: 'text' as const, text: 'Cited', providerOptions: { anthropic: { citations: [] } } },
    { type: 'tool-call' as const, toolCallId: 'toolu_fixture', toolName: 'view', input: {},
      providerOptions: { anthropic: { caller: { type: 'direct' }, toolsetName: 'fixture' } } },
  ] }], maxOutputTokens: 100 };
  expect(() => validateCall(profile, request)).not.toThrow();
  const compatible = { ...profile, provider: 'openai-compatible' as const };
  const tool = { ...request.prompt[0]!.content[1]!, providerOptions: { runbook: { thoughtSignature: 'signed' } } };
  expect(() => validateCall(compatible, { ...request, prompt: [{ role: 'assistant', content: [tool] }] })).not.toThrow();
  expect(() => validateCall(profile, { ...request, providerOptions: { anthropic: { caller: { type: 'direct' } } } })).toThrow();
});

it.each(['disconnect', 'timeout'])('settles %s against upstream input usage and relayed output bytes', async (kind) => {
  const abort = new AbortController();
  const deps = defaults();
  const { providerModel } = await import('../src/model/providers.ts');
  const { observeProviderFetch } = await import('../src/model/stream-fetch.ts');
  const result = await streamModel({ profile: 'sonnet', capability: 'cap',
    request: { prompt: [{ role: 'user', content: [{ type: 'text', text: 'fixture' }] }], maxOutputTokens: 500 } }, profile, {
    ...deps, signal: abort.signal, secrets: [],
    invoke: (call, tick, observe) => providerModel(profile, { api_key: 'fixture-secret' }, () =>
      observeProviderFetch(async () => new Response(new ReadableStream({
        start(controller) {
          const parts = [
            { type: 'message_start', message: { id: 'msg_partial', type: 'message', role: 'assistant', model: profile.model,
              content: [], stop_reason: null, stop_sequence: null,
              usage: { input_tokens: 100, cache_read_input_tokens: 15000, output_tokens: 1 } } },
            { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
            { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'abc'.repeat(250) } },
          ];
          controller.enqueue(new TextEncoder().encode(parts.map((part) => 'data: ' + JSON.stringify(part) + '\n\n').join('')));
        },
      }), { headers: { 'content-type': 'text/event-stream' } }), tick, observe)).doStream(call),
  });
  const reader = result.body.getReader();
  const decoder = new TextDecoder();
  for (;;) {
    const part = await reader.read();
    expect(part.done).toBe(false);
    if (decoder.decode(part.value).includes('abcabc')) break;
  }
  if (kind === 'disconnect') await reader.cancel();
  else {
    abort.abort(new Error('fixture timeout'));
    expect((await reader.read()).done).toBe(true);
  }
  await vi.waitFor(() => expect(deps.settle).toHaveBeenCalledWith(expect.objectContaining({
    usage: expect.objectContaining({ input_tokens: 15100, output_tokens: 250 }),
  })));
});

it('signals a permanent upstream error to the worker without repeating the request', async () => {
  const deps = defaults();
  const invoke = vi.fn().mockRejectedValue({ statusCode: 400, message: 'fixture sensitive error detail' });
  const result = await streamModel({ profile: 'sonnet', capability: 'cap',
    request: { prompt: [{ role: 'system', content: 'fixture' }], maxOutputTokens: 100 } }, profile, { ...deps, invoke });
  const wire = await new Response(result.body).text();
  expect(wire).toContain('model.permanent_failure');
  expect(wire).not.toContain('sensitive');
  expect(invoke).toHaveBeenCalledTimes(1);
});

it('uses Haiku long-context rates only when total input exceeds 100K, including cache hits', () => {
  const price = { ...profile, model: 'claude-haiku-5-5', inputUsdPerMillion: 0.1, outputUsdPerMillion: 0.5,
    cacheReadUsdPerMillion: 0.01, cacheWriteUsdPerMillion: 0.2, longContext: { aboveInputTokens: 100_000, multiplier: 5 } };
  const tokens = (total: number) => ({ ...usage, inputTokens: { total, cacheRead: total - 100, noCache: 100, cacheWrite: 0 } });
  expect(usageCost(price, tokens(100_000))).toBeCloseTo(0.001044, 9);
  expect(usageCost(price, tokens(100_001))).toBeCloseTo(0.00522005, 9);
});

it.each(['anthropic', 'vertex'] as const)('round trips %s wire requests with signed thinking and moving cache breaks', async (provider) => {
  const { claudeSettings } = await import('@runbook/runner-shared/claude-settings');
  const { modelCachePricing } = await import('@runbook/runner-shared/model-pricing');
  const endpoint = 'https://aiplatform.googleapis.com/v1/projects/angler-dev/locations/global/publishers/anthropic/models';
  const selected: ModelProfile = { ...profile, provider, ...modelCachePricing(provider, profile.model, 2),
    ...(provider === 'vertex' && { endpoint }) };
  const { runNativeAgent } = await import('../../worker/src/loop/native.ts');
  const { providerModel } = await import('../src/model/providers.ts');
  const { observeProviderFetch } = await import('../src/model/stream-fetch.ts');
  const deps = defaults();
  const upstreamRequests: unknown[] = [];
  const model = gatewayModel(profile.model, {
    gatewayUrl: 'http://fixture', streaming: true, capability: () => 'cap', processToken: 'fixture', maxOutputTokens: 1000,
    async fetch(_url, init) {
      const stream = await streamModel(JSON.parse(String(init?.body)), selected, {
        ...deps,
        invoke: (call, tick, observe) => providerModel(selected, provider === 'vertex' ? { access_token: 'fixture-secret' } : { api_key: 'fixture-secret' }, () =>
          observeProviderFetch(async (url, request) => {
            const headers = new Headers(request?.headers);
            if (provider === 'vertex') {
              expect(String(url)).toBe(endpoint + '/' + selected.model + ':streamRawPredict');
              expect(headers.get('authorization')).toBe('Bearer fixture-secret');
              expect(headers.has('x-api-key')).toBe(false);
              expect(headers.get('anthropic-beta')).not.toContain('mid-conversation-system-2026-04-07');
            }
            if (provider === 'anthropic' && upstreamRequests.length === 3) {
              expect(headers.get('anthropic-beta')).toContain('mid-conversation-system-2026-04-07');
            }
            expect(headers.get('anthropic-beta')).toContain('context-management-2025-06-27');
            expect(headers.get('anthropic-beta')).toContain('task-budgets-2026-03-13');
            expect(headers.get('anthropic-beta')).toContain('thinking-display-updates-2026-08-18');
            upstreamRequests.push(JSON.parse(String(request?.body)));
            const step = upstreamRequests.length;
            const first = step <= 3;
            const tool = first ? 'view' : 'submit_result';
            const toolId = first ? 'toolu_view_' + step : 'toolu_submit';
            const blocks = [
              { type: 'message_start', message: { id: 'msg_' + tool, type: 'message', role: 'assistant',
                model: profile.model, content: [], stop_reason: null, stop_sequence: null,
                usage: { input_tokens: 100, output_tokens: 0, cache_read_input_tokens: 500, cache_creation_input_tokens: 200 } } },
              { type: 'content_block_start', index: 0, content_block: { type: 'thinking', thinking: '' } },
              { type: 'content_block_delta', index: 0, delta: { type: 'thinking_delta', thinking: 'fixture thinking' } },
              { type: 'content_block_delta', index: 0, delta: { type: 'signature_delta', signature: 'signed-fixture' } },
              { type: 'content_block_stop', index: 0 },
              { type: 'content_block_start', index: 1, content_block: {
                type: 'tool_use', id: toolId, name: tool, input: {}, caller: { type: 'direct' },
              } },
              { type: 'content_block_delta', index: 1, delta: { type: 'input_json_delta',
                partial_json: first ? '{"path":"fixture"}' : JSON.stringify({
                  outputs: '{"ok":true}', summary: 'Read fixture', evidence: [{ claim: 'Read', tool_call_ids: ['call 1'] }], unverified: [],
                }),
              } },
              { type: 'content_block_stop', index: 1 },
              { type: 'message_delta', delta: { stop_reason: 'tool_use', stop_sequence: null }, usage: { output_tokens: 70 } },
              { type: 'message_stop' },
            ];
            return new Response(blocks.map((part) => 'data: ' + JSON.stringify(part) + '\n\n').join(''),
              { headers: { 'content-type': 'text/event-stream' } });
          }, tick, observe)).doStream(call),
      });
      return new Response(stream.body, { headers: stream.headers });
    },
  });
  const result = await runNativeAgent({ model, ...claudeSettings(provider, selected.model, 'default'), system: 'Fixture.', input: 'Read fixture.', outputSchema: undefined,
    signal: new AbortController().signal,
    limits: { maxTurns: 3, maxTokens: 1000, maxOutputTokens: 1000, timeoutMs: 2000, idleMs: 1000, toolMs: 100, stallMs: 500 },
    tools: [{ name: 'view', description: 'Read', schema: { type: 'object' }, async call() { return 'fixture'; } }],
    async storeResult(id) { return '/runbook-results/' + id.replace(' ', '-') + '.txt'; },
  });
  expect(result).toMatchObject({ status: 'succeeded', outputs: { ok: true } });
  expect(upstreamRequests).toHaveLength(4);
  const ttl = { type: 'ephemeral', ttl: '1h' };
  const settings = {
    thinking: { type: 'adaptive', display: 'updates' },
    output_config: { effort: 'medium', task_budget: { type: 'tokens', total: 99304 } },
    context_management: { edits: [
      { type: 'clear_thinking_20251015', keep: { type: 'thinking_turns', value: 2 } },
      { type: 'clear_tool_uses_20250919', trigger: { type: 'input_tokens', value: 100_000 },
        keep: { type: 'tool_uses', value: 3 }, clear_at_least: { type: 'input_tokens', value: 20_000 } },
    ] },
    ...(provider === 'vertex' ? { anthropic_version: 'vertex-2023-10-16' } : { model: selected.model }),
  };
  for (const [i, request] of upstreamRequests.entries()) {
    expect(request).toMatchObject(settings);
    if (provider === 'vertex') expect(request).not.toHaveProperty('model');
    const body = request as { system: unknown[]; messages: { role: string; content: Record<string, unknown>[] }[] };
    expect(body.system.at(-1)).toEqual({ type: 'text', text: 'Fixture.', cache_control: ttl });
    const results = body.messages.flatMap(({ content }) => content.filter(({ type }) => type === 'tool_result'));
    expect(results).toEqual(Array.from({ length: i }, (_, j) => ({
      type: 'tool_result', tool_use_id: 'toolu_view_' + (j + 1), content: '[call ' + (j + 1) + ']\nfixture\n[Saved result: /runbook-results/call-' + (j + 1) + '.txt]',
      ...(j >= i - 2 && { cache_control: ttl }),
    })));
    expect(JSON.stringify(request).match(/"cache_control":/g)?.length).toBe(1 + Math.min(i, 2));
    if (i > 0) expect(body.messages[1]?.content).toEqual([
      { type: 'thinking', thinking: 'fixture thinking', signature: 'signed-fixture' },
      { type: 'tool_use', id: 'toolu_view_1', name: 'view', input: { path: 'fixture' }, caller: { type: 'direct' } },
    ]);
  }
  expect(upstreamRequests[3]).toMatchObject({ messages: expect.arrayContaining([{
    role: 'system', content: [{ type: 'text', text: 'This tool call has returned the same result three times. Try a different approach.\n\n' +
      'This is your final turn. Only submit_result and report_blocked can be executed.' }],
  }]) });
  expect(deps.settle).toHaveBeenCalledWith(expect.objectContaining({
    // Sonnet 5.5 reads cache at $0.10 per million on the Claude API and Vertex.
    actual_usd: expect.closeTo((100 * 2 + 500 * 0.1 + 200 * 4 + 70 * 10) / 1_000_000, 10),
    usage: expect.objectContaining({ cached_tokens: 500, cache_write_tokens: 200 }),
  }));
});

it('keeps a worker stream alive throughout Retry-After backoff', async () => {
  const { nativeTurn } = await import('../../worker/src/loop/native-turn.ts');
  const deps = defaults();
  const request = { prompt: [{ role: 'system' as const, content: 'fixture' }], maxOutputTokens: 100 };
  const invoke = vi.fn()
    .mockRejectedValueOnce({ statusCode: 429, responseHeaders: { 'retry-after': '0.65' } })
    .mockImplementation(async () => ({ stream: new ReadableStream({ start(controller) {
      controller.enqueue(finish);
      controller.close();
    } }) }));
  const model = gatewayModel(profile.model, {
    gatewayUrl: 'http://fixture', streaming: true, capability: () => 'cap', processToken: 'fixture', maxOutputTokens: 100,
    async fetch(_url, init) {
      const result = await streamModel(JSON.parse(String(init?.body)), profile, { ...deps, invoke });
      return new Response(result.body, { headers: result.headers });
    },
  });
  const activity = vi.fn();
  await expect(nativeTurn(model, request, deps.signal, 450, activity, () => undefined, () => undefined))
    .resolves.toMatchObject({ finish });
  expect(invoke).toHaveBeenCalledTimes(2);
  expect(activity.mock.calls.length).toBeGreaterThanOrEqual(3);
});

it('clamps a finish turn to what the reservation can afford', async () => {
  const deps = defaults();
  const request = { prompt: [{ role: 'system' as const, content: 'fixture' }], maxOutputTokens: 4096 };
  const inputUsd = reservationInput(profile, request);
  const result = await streamModel({ profile: profile.model, capability: 'cap', request, endgame: true }, profile, {
    ...deps,
    async reserve(hold) {
      expect(hold.headroom_usd).toBeUndefined();
      expect(hold.amount_usd).toBeCloseTo(inputUsd + 4096 * 10 / 1_000_000);
      return { reservation_id: 'small-hold', amount_usd: inputUsd + 128 * 10 / 1_000_000 };
    },
    async invoke(call) {
      expect(call.maxOutputTokens).toBe(128);
      return { stream: new ReadableStream({ start(controller) { controller.enqueue(finish); controller.close(); } }) };
    },
  });
  expect(await new Response(result.body).text()).toContain('tool_use');
});

it('reserves finish-turn input room for the next tool batch', async () => {
  const request = { prompt: [{ role: 'system' as const, content: 'fixture' }], maxOutputTokens: 2048 };
  const next = { ...request, prompt: [{ role: 'system' as const, content: 'fixture' + 'x'.repeat(96 * 1024) }] };
  await expect(streamModel({ profile: profile.model, capability: 'cap', request, endgame: false }, profile, {
    ...defaults(),
    async reserve(hold) {
      expect(hold.headroom_usd).toBeGreaterThanOrEqual(reservationInput(profile, next) + 4096 * 10 / 1_000_000);
      return { level: 'step' };
    },
    async invoke() { throw new Error('should not invoke'); },
  })).rejects.toMatchObject({ code: 'model.budget_reached' });
});

it.each([
  ['claude-haiku-5-5', 'cheap', undefined, { thinking: { type: 'adaptive', display: 'summarized' }, output_config: { effort: 'low' } }],
  ['claude-sonnet-5-5', 'default', undefined, { thinking: { type: 'adaptive', display: 'updates' }, output_config: { effort: 'medium' } }],
  ['claude-opus-5-5', 'deep', 'medium', { thinking: { type: 'adaptive', display: 'updates' }, output_config: { effort: 'medium' } }],
  ['claude-opus-4-6', 'deep', undefined, { thinking: { type: 'adaptive', display: 'summarized' }, output_config: { effort: 'high' } }],
  ['claude-haiku-4-5-20251001', 'cheap', undefined, {}],
] as const)('sends supported thinking and effort on the %s wire', async (model, name, override, expected) => {
  const { providerModel } = await import('../src/model/providers.ts');
  const { claudeSettings } = await import('@runbook/runner-shared/claude-settings');
  const options = claudeSettings('anthropic', model, name, override).modelOptions satisfies AnthropicProviderOptions;
  const wire = vi.fn(async (_input: Parameters<typeof fetch>[0], request?: RequestInit) => {
    const { thinking, output_config } = JSON.parse(String(request?.body));
    expect({ ...(thinking && { thinking }), ...(output_config && { output_config }) }).toEqual(expected);
    const events = [
      { type: 'message_start', message: { id: 'msg_fixture', type: 'message', role: 'assistant', model,
        content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 20, output_tokens: 0 } } },
      { type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 1 } },
      { type: 'message_stop' },
    ];
    return new Response(events.map((event) => 'data: ' + JSON.stringify(event) + '\n\n').join(''),
      { headers: { 'content-type': 'text/event-stream' } });
  });
  const response = await providerModel({ ...profile, model }, { api_key: 'fixture' }, () => wire).doStream({
    prompt: [{ role: 'user', content: [{ type: 'text', text: 'fixture' }] }],
    providerOptions: { anthropic: { ...options } }, maxOutputTokens: 2048,
  });
  const parts: LanguageModelV4StreamPart[] = [];
  for await (const part of response.stream) parts.push(part);
  expect(parts.at(-1)).toMatchObject({ type: 'finish', finishReason: { raw: 'end_turn' } });
  expect(wire).toHaveBeenCalledOnce();
});

it.each([400, 429, 500, 503, 529])('settles definitive provider rejection %s at zero', async (statusCode) => {
  const deps = defaults();
  const result = await streamModel({ profile: profile.model, capability: 'cap', attemptLimit: 1,
    request: { prompt: [{ role: 'system', content: 'fixture' }], maxOutputTokens: 100 } }, profile, {
    ...deps, async invoke(_call, _activity, _observe, dispatched) {
      dispatched();
      throw { statusCode };
    },
  });
  await new Response(result.body).text();
  expect(deps.settle).toHaveBeenCalledWith({ reservation_id: 'fixture', actual_usd: 0, usage: {} });
});

it.each([false, true])('keeps dispatched output-only zero usage pending (prior 5xx: %s)', async (retry) => {
  const deps = defaults();
  let attempts = 0;
  const result = await streamModel({ profile: profile.model, capability: 'cap', attemptLimit: 2,
    request: { prompt: [{ role: 'system', content: 'fixture' }], maxOutputTokens: 100 } }, profile, {
    ...deps, async invoke(_call, _activity, observe, dispatched) {
      dispatched();
      if (++attempts === 1 && retry) throw { statusCode: 500 };
      observe(profile.model, { inputTokens: { total: undefined, noCache: undefined, cacheRead: undefined, cacheWrite: undefined },
        outputTokens: { total: 0, text: undefined, reasoning: undefined } });
      return { stream: new ReadableStream({ start(controller) { controller.close(); } }) };
    },
  });
  await new Response(result.body).text();
  expect(attempts).toBe(retry ? 2 : 1);
  expect(deps.settle).toHaveBeenCalledWith(expect.objectContaining({ actual_usd: 0.1, usage_pending: true }));
});

it('keeps a broken 2xx stream pending and records observed input usage', async () => {
  const deps = defaults();
  const { providerModel } = await import('../src/model/providers.ts');
  const { observeProviderFetch, streamResponseLimit } = await import('../src/model/stream-fetch.ts');
  const limited = { ...profile, maxResponseBytes: 512 };
  const upstream = [
    { type: 'message_start', message: { id: 'msg_limited', type: 'message', role: 'assistant', model: profile.model,
      content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 100, output_tokens: 0 } } },
    { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
    { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'overflow'.repeat(100) } },
    { type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 200 } },
    { type: 'message_stop' },
  ];
  const result = await streamModel({ profile: profile.model, capability: 'cap', attemptLimit: 1,
    request: { prompt: [{ role: 'system', content: 'fixture' }], maxOutputTokens: 100 } }, limited, {
    ...deps,
    invoke: (call, tick, observe, dispatched, respond) => providerModel(limited, { api_key: 'fixture-secret' }, () =>
      observeProviderFetch(async () => new Response(new ReadableStream({
        start(controller) {
          for (const event of upstream) controller.enqueue(new TextEncoder().encode('data: ' + JSON.stringify(event) + '\n\n'));
          controller.close();
        },
      }), { headers: { 'content-type': 'text/event-stream' } }), tick, observe, dispatched, streamResponseLimit(limited), respond)).doStream(call),
  });
  const wire = await new Response(result.body).text();
  expect(wire).toContain('model.call_failed');
  expect(wire).not.toContain('overflow');
  expect(deps.settle).toHaveBeenCalledWith({
    reservation_id: 'fixture', actual_usd: 0.1, usage_pending: true,
    usage: { input_tokens: 100, output_tokens: 0, cached_tokens: 0, cache_write_tokens: 0 },
  });
});

it('settles only measured usage when a 503 is followed by success', async () => {
  const deps = defaults();
  const invoke = vi.fn().mockRejectedValueOnce({ statusCode: 503, responseHeaders: { 'retry-after': '0.01' } });
  const result = await streamModel({ profile: profile.model, capability: 'cap', attemptLimit: 2,
    request: { prompt: [{ role: 'system', content: 'fixture' }], maxOutputTokens: 100 } }, profile, {
    ...deps, async invoke(call, activity, observe, dispatch) {
      dispatch();
      await invoke(call, activity);
      observe(profile.model, {
        inputTokens: { total: 1000, noCache: 1000, cacheRead: undefined, cacheWrite: undefined },
        outputTokens: { total: 10, text: 10, reasoning: undefined },
      });
      return { stream: new ReadableStream({ start(controller) {
        controller.enqueue({ type: 'finish', finishReason: { unified: 'stop', raw: 'stop' }, usage: {
          inputTokens: { total: 1000, noCache: 1000, cacheRead: undefined, cacheWrite: undefined },
          outputTokens: { total: 10, text: 10, reasoning: undefined },
        } });
        controller.close();
      } }) };
    },
  });
  await new Response(result.body).text();
  expect(invoke).toHaveBeenCalledTimes(2);
  expect(deps.settle).toHaveBeenCalledWith({
    reservation_id: 'fixture', actual_usd: 0.0021,
    usage: { input_tokens: 1000, output_tokens: 10, cached_tokens: 0, cache_write_tokens: 0 },
  });
});

it.each(['claude-haiku-5-5@20261001', 'publishers/anthropic/models/claude-haiku-5-5@20261001'])(
  'prices long-context Vertex Haiku requests with model ID %s', async (id) => {
    const { modelCachePricing } = await import('@runbook/runner-shared/model-pricing');
    const { modelCost, estimateWorstCase } = await import('../src/model/pricing.ts');
    const rates = { inputUsdPerMillion: 1.1, outputUsdPerMillion: 5.5, ...modelCachePricing('vertex', id, 1.1) };
    expect(modelCost(rates, 150_000, 1000)).toBeCloseTo(0.8525);
    expect(estimateWorstCase(rates, { prompt: 'x'.repeat(600_000), max_tokens: 1000 })).toBeCloseTo(6.638764);
  },
);
