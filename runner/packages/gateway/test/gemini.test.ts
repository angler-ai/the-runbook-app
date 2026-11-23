import { vertexBinding } from '@runbook/runner-shared/vertex-binding';
import { expect, it } from 'vitest';
import type { LanguageModelV4CallOptions, LanguageModelV4StreamPart } from '@ai-sdk/provider';
import { providerModel } from '../src/model/providers.ts';
import { streamModel, validateCall } from '../src/model/stream.ts';

const endpoint = 'https://aiplatform.googleapis.com/v1/projects/angler-dev/locations/global/publishers/google/models';
const profile = { provider: 'vertex-gemini', endpoint, model: 'gemini-4-hydrogen',
  credentialId: 'test', maxOutputTokens: 4096, inputUsdPerMillion: 1.5, outputUsdPerMillion: 7.5 } as const;
const call: LanguageModelV4CallOptions = { maxOutputTokens: 4096,
  providerOptions: { googleVertex: { thinkingConfig: { thinkingLevel: 'low' } } },
  prompt: [{ role: 'system', content: 'Inspect the evidence.' }, { role: 'user', content: [{ type: 'text', text: 'Read a and b' }] }],
  tools: [{ type: 'function', name: 'read', inputSchema: { type: 'object', properties: { path: { type: 'string' } } } }],
};
const toolCalls = [{ functionCall: { id: 'call-a', name: 'read', args: { path: 'a' } }, thoughtSignature: 'opaque+/A=' },
  { functionCall: { id: 'call-b', name: 'read', args: { path: 'b' } } }];

it('round-trips parallel Vertex calls and preserves opaque signatures, IDs and raw stop reason', async () => {
  const requests: Record<string, unknown>[] = [];
  const binding = vertexBinding('vertex-gemini', 'angler-dev', 'global')!;
  const model = providerModel({ ...profile, vertexBinding: binding }, { access_token: 'fixture-token' }, () => async (url, init) => {
    expect(String(url)).toBe(`${endpoint}/gemini-4-hydrogen:streamGenerateContent?alt=sse`);
    const headers = new Headers(init?.headers);
    expect(headers.get('authorization')).toBe('Bearer fixture-token');
    expect(headers.has('x-goog-user-project')).toBe(false);
    expect(headers.has('x-goog-api-key')).toBe(false);
    requests.push(JSON.parse(String(init?.body)));
    const parts = requests.length === 1 ? toolCalls : [{ text: 'Done.', thoughtSignature: 'opaque+/B=' }];
    return new Response('data: ' + JSON.stringify({ candidates: [{ content: { role: 'model', parts }, finishReason: 'STOP' }],
      usageMetadata: { promptTokenCount: 8192, cachedContentTokenCount: 4096, candidatesTokenCount: 8, thoughtsTokenCount: 41 },
      modelVersion: profile.model }) + '\n\n', { headers: { 'content-type': 'text/event-stream' } });
  });
  async function read(request: LanguageModelV4CallOptions) {
    validateCall(profile, request);
    const reader = (await model.doStream(request)).stream.getReader();
    const parts: LanguageModelV4StreamPart[] = [];
    while (true) {
      const next = await reader.read();
      if (next.done) return parts;
      parts.push(next.value);
    }
  }
  const first = await read(call);
  expect(first.at(-1)).toMatchObject({ type: 'finish', finishReason: { raw: 'STOP', unified: 'tool-calls' },
    usage: { inputTokens: { total: 8192, cacheRead: 4096 }, outputTokens: { total: 49, reasoning: 41 } } });
  const calls = first.filter((part) => part.type === 'tool-call');
  await read({ ...call, prompt: [...call.prompt,
    { role: 'assistant', content: calls.map(({ toolCallId, toolName, input, providerMetadata }) =>
      ({ type: 'tool-call', toolCallId, toolName, input: JSON.parse(input), providerOptions: providerMetadata })) },
    { role: 'tool', content: calls.map(({ toolCallId, toolName }) =>
      ({ type: 'tool-result', toolCallId, toolName, output: { type: 'text', value: 'ok' } })) },
  ] });
  expect(requests[0]?.generationConfig).toMatchObject({ thinkingConfig: { thinkingLevel: 'LOW' } });
  expect(requests[0]?.systemInstruction).toBeDefined();
  expect(requests[1]?.systemInstruction).toEqual(requests[0]?.systemInstruction);
  expect(requests[0]?.tools).toBeDefined();
  expect(requests[1]?.tools).toEqual(requests[0]?.tools);
  expect((requests[1]?.contents as unknown[])[0]).toEqual((requests[0]?.contents as unknown[])[0]);
  expect(requests[1]?.contents).toMatchObject([
    { role: 'user' }, { role: 'model', parts: toolCalls },
    { role: 'user', parts: [{ functionResponse: { id: 'call-a', name: 'read' } },
      { functionResponse: { id: 'call-b', name: 'read' } }] },
  ]);
});

it.each([{ cachedContent: 'unbilled' }, { thinkingConfig: { thinkingBudget: 32 } },
  { thinkingConfig: { thinkingLevel: 'minimal' } }, { requestType: 'priority' }])('denies Gemini options %j', (options) => {
  expect(() => validateCall(profile, { ...call, providerOptions: { googleVertex: options } })).toThrow('model option is not allowed');
});

it('keeps the live MAX_TOKENS shape as a bounded stop even when it carries a continuation token', async () => {
  let requests = 0;
  const model = providerModel(profile, { api_key: 'fixture-token' }, () => async (_url, init) => {
    expect(new Headers(init?.headers).get('x-goog-user-project')).toBe('angler-dev');
    requests++;
    const event = { candidates: [{ content: { role: 'model', parts: [{ text: '' }] },
      finishReason: 'MAX_TOKENS', continuationToken: 'opaque-token' }],
    usageMetadata: { promptTokenCount: 98, candidatesTokenCount: 6, totalTokenCount: 148, thoughtsTokenCount: 44 },
    modelVersion: profile.model };
    return new Response('data: ' + JSON.stringify(event) + '\n\n', { headers: { 'content-type': 'text/event-stream' } });
  });
  const stream = (await model.doStream({ ...call, maxOutputTokens: 50 })).stream;
  const parts: LanguageModelV4StreamPart[] = [];
  const reader = stream.getReader();
  for (let next = await reader.read(); !next.done; next = await reader.read()) parts.push(next.value);
  expect(requests).toBe(1);
  expect(parts.at(-1)).toMatchObject({ finishReason: { raw: 'MAX_TOKENS', unified: 'length' },
    usage: { outputTokens: { total: 50, reasoning: 44 } },
    providerMetadata: { googleVertex: { continuationToken: 'opaque-token' } },
  });
});


it('relays the served Vertex model alongside the unchanged finish part', async () => {
  const usage = { inputTokens: { total: 8192, noCache: 4096, cacheRead: 4096, cacheWrite: 0 },
    outputTokens: { total: 26, text: 6, reasoning: 20 } };
  const finish = { type: 'finish', finishReason: { raw: 'STOP', unified: 'stop' }, usage } as const;
  const result = await streamModel({ profile: 'default', capability: 'fixture', request: call }, { ...profile, cacheReadUsdPerMillion: 0.075 }, {
    signal: new AbortController().signal, availableMs: 1000, secrets: [],
    reserve: async () => ({ reservation_id: 'fixture' }),
    async settle(settlement) {
      expect(settlement.usage).toMatchObject({ input_tokens: 8192, cached_tokens: 4096 });
      expect(settlement.actual_usd).toBeCloseTo((4096 * 1.5 + 4096 * 0.075 + 26 * 7.5) / 1_000_000);
    },
    finished() {},
    async invoke(_call, tick, observe) {
      tick();
      observe(profile.model, usage);
      return { stream: new ReadableStream<LanguageModelV4StreamPart>({ start(controller) {
        controller.enqueue({ type: 'response-metadata', id: 'vertex-response' });
        controller.enqueue(finish);
        controller.close();
      } }) };
    },
  });
  const wire = await new Response(result.body).text();
  expect(wire).toContain(JSON.stringify({ type: 'response-metadata', id: 'vertex-response' }));
  expect(wire).toContain(JSON.stringify({ type: 'response-metadata', modelId: profile.model }));
  expect(wire).toContain(JSON.stringify(finish));
});
