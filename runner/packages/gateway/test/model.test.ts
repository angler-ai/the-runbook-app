import { expect, it, vi } from 'vitest';
import { generateModel } from '../src/model/endpoint.ts';
import { anthropicPrompt, invokeModel } from '../src/model/providers.ts';

const profile = {
  credentialId: 'credential', provider: 'anthropic' as const, model: 'test-model',
  endpoint: 'https://provider.example', inputUsdPerMillion: 10, outputUsdPerMillion: 20,
  maxOutputTokens: 1024,
};
const secret = 'provider-secret-fixture';
function setup() {
  const order: string[] = [];
  const deps = {
    resolve: vi.fn(async () => profile),
    material: () => ({
      fields: { api_key: secret },
      binding: { endpoint: profile.endpoint, provider: profile.provider },
    }),
    reserve: vi.fn(async () => { order.push('reserve'); return { reservation_id: 'reservation' }; }),
    invoke: vi.fn(async () => {
      order.push('call');
      return { text: secret, inputTokens: 10, outputTokens: 2 };
    }),
    settle: vi.fn(async () => { order.push('settle'); }),
    redact: (value: string, values: string[]) => values.reduce((s, match) => s.replaceAll(match, '[REDACTED]'), value),
  };
  return { deps, order };
}
const input = { capability: 'capability', profile: 'default', request: { prompt: 'test prompt', max_tokens: 5 } };

it('reserves_before_call_and_settles_after', async () => {
  const { deps, order } = setup();
  await generateModel(deps, input);
  expect(order).toEqual(['reserve', 'call', 'settle']);
  expect(deps.settle).toHaveBeenCalledWith(expect.objectContaining({
    reservation_id: 'reservation', actual_usd: 0.00014, usage: { input_tokens: 10, output_tokens: 2 },
  }));
});
it('refuses_call_when_ledger_returns_402', async () => {
  const { deps } = setup();
  const blocked = { ...deps, reserve: async () => ({ level: 'project' }) };
  await expect(generateModel(blocked, input)).rejects.toMatchObject({ status: 402, message: 'budget reached: project' });
  expect(deps.invoke).not.toHaveBeenCalled();
});
it('provider_key_never_reaches_worker', async () => {
  const { deps } = setup();
  expect(JSON.stringify(await generateModel(deps, input))).not.toContain(secret);
  deps.invoke.mockRejectedValueOnce(new Error(secret));
  await expect(generateModel(deps, input)).rejects.toMatchObject({ message: 'model call failed' });
});
it('prompt_caching_enabled_for_anthropic_by_default', () => {
  expect(anthropicPrompt('test')).toEqual([{
    role: 'user', content: 'test', providerOptions: { anthropic: { cacheControl: { type: 'ephemeral' } } },
  }]);
});
it('unknown_profile_for_agent_refused', async () => {
  const { deps } = setup();
  await expect(generateModel({ ...deps, resolve: async () => undefined }, input)).rejects.toMatchObject({ status: 403 });
  expect(deps.reserve).not.toHaveBeenCalled();
});

it('model_http_requires_explicit_operator_opt_in', async () => {
  const insecure = { ...profile, provider: 'openai-compatible' as const, endpoint: 'http://provider.example' };
  vi.stubEnv('RUNNER_ALLOW_HTTP_MODEL_ENDPOINTS', undefined);
  try {
    await expect(invokeModel(insecure, input.request, {})).rejects.toThrow('invalid model endpoint');
    vi.stubEnv('RUNNER_ALLOW_HTTP_MODEL_ENDPOINTS', 'true');
    await expect(invokeModel(insecure, input.request, {})).rejects.toThrow('model credential unavailable');
    await expect(invokeModel(profile, input.request, {})).rejects.toThrow('model credential unavailable');
  } finally {
    vi.unstubAllEnvs();
  }
});

it('refuses attached model identity before reserving or invoking', async () => {
  const { deps } = setup();
  const resolve = async () => ({ ...profile, provider: 'vertex' as const });
  await expect(generateModel({ ...deps, resolve }, input))
    .rejects.toMatchObject({ code: 'model.attached_identity_disabled' });
  expect(deps.reserve).not.toHaveBeenCalled();
  expect(deps.invoke).not.toHaveBeenCalled();
});

it('enforces the signed profile, output cap, and absolute deadline before any provider work', async () => {
  const { modelRoute } = await import('../src/model/routes.ts');
  const { MaterialStore } = await import('../src/material.ts');
  const { loadRedaction } = await import('@runbook/runner-shared/redact');
  const now = Date.now();
  const claims = { runner_id: 'r', model_profile: 'fast', max_tokens: 4, timeout_ms: 1000, step_deadline_ms: now + 1000,
    exp: Math.ceil(now / 1000) + 60 } as Awaited<ReturnType<Parameters<typeof modelRoute>[1]['verifyCapability']>>;
  const route = modelRoute({ role: 'gateway', listen: { host: '', port: 0 }, urls: {},
    inboundTokens: {}, outboundTokens: {} },
  { verifyCapability: async () => claims },
  new MaterialStore(), await loadRedaction());
  const call = (profile: string, max_tokens: number, runnerId = 'r') => route.handle({
    caller: 'worker', runnerId, params: {}, body: { ...input, profile, request: { prompt: 'test', max_tokens } },
  });
  await expect(call('fast', 4, 'other-runner')).rejects.toMatchObject({ code: 'model.runner_denied' });
  await expect(call('deep', 4)).rejects.toMatchObject({ code: 'model.profile_denied' });
  await expect(call('fast', 5)).rejects.toMatchObject({ code: 'model.invalid' });
  claims.step_deadline_ms = now - 1;
  await expect(call('fast', 4)).rejects.toMatchObject({ code: 'model.claim_ended' });
});

it('reserves the full prompt through release renewal and scans only substantial secret fields', async () => {
  const { modelRoute } = await import('../src/model/routes.ts');
  const { MaterialStore } = await import('../src/material.ts');
  const { loadRedaction } = await import('@runbook/runner-shared/redact');
  const providers = await import('../src/model/providers.ts');
  const now = Date.now();
  const claims = { kind: 'capability', sub: 'step', claim_id: 'claim', generation: 1, runner_id: 'runner',
    exp: Math.floor(now / 1000) + 60, model_profile: 'default' } as Awaited<ReturnType<Parameters<typeof modelRoute>[1]['verifyCapability']>>;
  const store = new MaterialStore();
  const first = { claim_id: claims.claim_id, generation: claims.generation, runner_id: claims.runner_id,
    release_id: 'first', step_run_id: claims.sub, credential_id: profile.credentialId, delivery: 'model' as const,
    binding: { endpoint: profile.endpoint, provider: profile.provider },
    material: { fields: { api_key: secret, username: 'fixture-username' } }, expires_at: new Date(now + 60_000).toISOString() };
  store.put(first);
  store.put({ ...first, release_id: 'proxy', credential_id: 'proxy', delivery: 'proxy',
    material: { fields: { key: 'east' } } });
  const route = modelRoute({ role: 'gateway', listen: { host: '', port: 0 }, urls: { api: 'http://fixture' },
    inboundTokens: {}, outboundTokens: {} }, { verifyCapability: async () => claims }, store, await loadRedaction(), true);
  const reserves: number[] = [];
  vi.stubGlobal('fetch', vi.fn(async (url: string, init: RequestInit) => {
    if (url.endsWith('/model/profile')) return Response.json({ ...profile, cacheReadUsdPerMillion: 1, cacheWriteUsdPerMillion: 20 });
    if (url.endsWith('/ledger/reserve')) {
      const { amount_usd } = JSON.parse(String(init.body));
      reserves.push(amount_usd);
      return Response.json({ reservation_id: 'hold', amount_usd });
    }
    return new Response(null);
  }));
  let text = 'Configured fixture-username in east.';
  const upstream = vi.spyOn(providers, 'providerModel').mockImplementation(() => ({
    specificationVersion: 'v4', provider: 'fixture', modelId: profile.model, supportedUrls: {},
    async doGenerate() { throw new Error('Unexpected generate'); },
    async doStream() {
    return { stream: new ReadableStream({ start(controller) {
      controller.enqueue({ type: 'text-delta', id: 't', delta: text });
      controller.enqueue({ type: 'finish', finishReason: { unified: 'stop', raw: 'end_turn' },
        usage: { inputTokens: { total: 14_000, noCache: 0, cacheRead: 14_000, cacheWrite: 0 },
          outputTokens: { total: 10, reasoning: 0, text: 10 } } });
      controller.close();
    } }) };
  } }));
  const prompt = [{ role: 'system', content: 'fixture '.repeat(5000) }];
  const call = async () => new Response((await route.handle({ params: {}, caller: 'worker', runnerId: 'runner', body: {
    capability: 'cap', profile: 'default', request: { prompt, maxOutputTokens: 100 },
  } })).body as ReadableStream).text();
  try {
    const initial = await call();
    expect(initial).toContain(text);
    expect(initial).not.toContain('secret_in_model_output');
    store.delete(first.release_id);
    store.put({ ...first, release_id: 'renewed' });
    prompt.push({ role: 'system', content: 'New turn.' });
    await call();
    expect(reserves[1]).toBeGreaterThan(reserves[0]!);
    text = secret;
    const blocked = await call();
    expect(blocked).toContain('secret_in_model_output');
    expect(blocked).not.toContain(secret);
  } finally {
    upstream.mockRestore();
    vi.unstubAllGlobals();
    store.clear();
  }
});

it('reserves the maximum input rate and settles Anthropic cache writes', async () => {
  const { deps } = setup();
  const pricedProfile = { ...profile, cacheWriteUsdPerMillion: 25, cacheReadUsdPerMillion: 1 };
  deps.resolve.mockResolvedValue(pricedProfile);
  const invoke = async () => ({ text: 'answer', inputTokens: 100, outputTokens: 2, cacheWriteTokens: 60, cacheReadTokens: 20 });
  await generateModel({ ...deps, invoke }, input);
  expect(deps.reserve.mock.calls[0]).toEqual([{
    capability: 'capability', kind: 'model', amount_usd: ((1024 + 11) * 25 + 5 * 20) / 1_000_000,
  }]);
  expect(deps.settle).toHaveBeenCalledWith({
    reservation_id: 'reservation', actual_usd: (20 * 10 + 60 * 25 + 20 * 1 + 2 * 20) / 1_000_000,
    usage: { input_tokens: 100, output_tokens: 2, cached_tokens: 20, cache_write_tokens: 60 },
  });
});

it('cancels a non-streaming provider body before the SDK can buffer beyond the byte limit', async () => {
  const egress = await import('../src/egress/fetch.ts');
  const chunk = new Uint8Array(1024 * 1024);
  let sent = 0;
  const cancel = vi.fn();
  const fetcher = vi.spyOn(egress, 'pinnedFetch').mockResolvedValueOnce(new Response(new ReadableStream({
    pull(controller) {
      if (sent++ < 20) controller.enqueue(chunk);
      else controller.close();
    },
    cancel,
  }), { headers: { 'content-type': 'application/json' } }));
  try {
    await expect(invokeModel(profile, input.request, { api_key: secret })).rejects.toThrow();
    await vi.waitFor(() => expect(cancel).toHaveBeenCalledOnce());
  } finally {
    fetcher.mockRestore();
  }
});

it('does not charge a reservation when the local egress policy blocks dispatch', async () => {
  const { deps } = setup();
  const local = { ...profile, endpoint: 'https://127.0.0.1' };
  await expect(generateModel({
    ...deps, resolve: async () => local, material: () => ({
      fields: { api_key: secret }, binding: { provider: local.provider, endpoint: local.endpoint },
    }),
    invoke: (profile, request, fields, dispatched, respond) => invokeModel(profile, request, fields, undefined, dispatched, respond),
  }, input)).rejects.toMatchObject({ code: 'model.call_failed' });
  expect(deps.settle).toHaveBeenCalledWith({ reservation_id: 'reservation', actual_usd: 0, usage: {} });
});

it('settles a non-streaming provider 503 at zero', async () => {
  const { deps } = setup();
  await expect(generateModel({ ...deps, async invoke(_profile, _request, _fields, dispatch, respond) {
    dispatch?.();
    respond?.(503);
    throw new Error('provider unavailable');
  } }, input)).rejects.toMatchObject({ code: 'model.call_failed' });
  expect(deps.settle).toHaveBeenCalledWith({ reservation_id: 'reservation', actual_usd: 0, usage: {} });
});
