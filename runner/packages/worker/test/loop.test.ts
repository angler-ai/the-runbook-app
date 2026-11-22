import { StepStopped } from '../src/loop/limits.ts';
import { expect, it, vi } from 'vitest';
import { gatewayModel } from '@runbook/runner-shared/gateway-model-provider';
import { compileOutputSchema, runAgent } from '../src/loop/loop.ts';

function fixture(responses: unknown[]) {
  const transport = vi.fn<typeof fetch>(async () => {
    const response = responses.shift();
    return response instanceof Response ? response : Response.json({
      text: JSON.stringify(response), usage: { input_tokens: 2, output_tokens: 3 },
    });
  });
  const setup = {
    model: gatewayModel('fixture', { gatewayUrl: 'http://fake-gateway', processToken: 'test', capability: () => 'cap',
      maxOutputTokens: 50, fetch: transport }),
    system: 'Agent instructions\nPinned skill text', input: 'Run data',
    outputSchema: { type: 'object', properties: { ok: { type: 'boolean' } }, required: ['ok'], additionalProperties: false },
    limits: { maxTurns: 5, maxTokens: 100, maxOutputTokens: 50, timeoutMs: 5000,
      idleMs: 5000, toolMs: 5000, stallMs: 5000 },
    signal: new AbortController().signal,
    tools: [{ name: 'read', description: 'Read a file', schema: { type: 'object' }, call: async () => { throw new Error('private error'); } }],
  };
  return { setup, transport };
}

it('reports_outputs_validated_against_schema', async () => {
  const f = fixture([{ outputs: { ok: 'not a boolean' } }, { outputs: { ok: true } }]);
  expect(await runAgent(f.setup)).toMatchObject({ status: 'succeeded', outputs: { ok: true } });
  expect(f.transport).toHaveBeenCalledTimes(2);
  const request = JSON.parse(f.transport.mock.calls[0]?.[1]?.body as string) as { request: { messages: unknown[] } };
  expect(request.request.messages[0]).toMatchObject({ role: 'system', content: expect.stringContaining('Pinned skill text') });
});

it('max_turns_enforced', async () => {
  const f = fixture([{ outputs: { ok: 'invalid' } }, { outputs: { ok: true } }]);
  expect(await runAgent({ ...f.setup, limits: { ...f.setup.limits, maxTurns: 1 } }))
    .toMatchObject({ status: 'failed', summary: 'max_turns' });
  expect(f.transport).toHaveBeenCalledOnce();
});

it('budget_402_ends_step_with_level', async () => {
  const f = fixture([Response.json({ error: { message: 'budget reached: project:day' } }, { status: 402 })]);
  expect(await runAgent(f.setup)).toMatchObject({ status: 'failed', summary: 'limit:project:day' });
});

it('tool_error_returned_to_model_not_thrown', async () => {
  const f = fixture([{ tool: 'read', arguments: { path: 'missing' } }, { outputs: { ok: true } }]);
  expect(await runAgent(f.setup)).toMatchObject({ status: 'succeeded' });
  expect(f.transport.mock.calls[1]?.[1]?.body).toContain('tool_failed');
  expect(f.transport.mock.calls[1]?.[1]?.body).not.toContain('private error');
});

it('idle_step_ends_with_idle', async () => {
  const f = fixture([]);
  f.transport.mockImplementation(async () => new Promise(() => undefined));
  expect(await runAgent({ ...f.setup, limits: { ...f.setup.limits, idleMs: 10 } }))
    .toMatchObject({ status: 'failed', summary: 'idle' });
});

it('rejects oversized output schema patterns before consulting the model', async () => {
  const f = fixture([{ outputs: { answer: 'x'.repeat(129) } }]);
  const result = await runAgent({ ...f.setup, outputSchema: {
    type: 'object', properties: { answer: { type: 'string', pattern: 'x'.repeat(129) } },
  } });
  expect(result).toMatchObject({ summary: 'invalid_output_schema' });
  expect(f.transport).not.toHaveBeenCalled();
});

it('compiles schemas with reused identifiers independently', async () => {
  const f = fixture([{ outputs: { ok: true } }, { outputs: { ok: 'yes' } }]);
  const schema = (type: string) => ({ $id: 'fixture', type: 'object', properties: { ok: { type } } });
  expect(await runAgent({ ...f.setup, outputSchema: schema('boolean') })).toMatchObject({ status: 'succeeded' });
  expect(await runAgent({ ...f.setup, outputSchema: schema('string') })).toMatchObject({ status: 'succeeded' });
});

it('a lost sandbox ends the step before the next model call', async () => {
  const f = fixture([{ tool: 'read', arguments: { path: 'missing' } }, { outputs: { ok: true } }]);
  f.setup.tools[0]!.call = async () => { throw new StepStopped('sandbox_gone'); };
  expect(await runAgent(f.setup)).toMatchObject({ summary: 'sandbox_gone' });
  expect(f.transport).toHaveBeenCalledOnce();
});

// Release gate 2026-11-21: the core emits output schemas as JSON Schema
// 2020-12 (`$schema` set, runbook-spec schema.rs), and every runner step with
// outputs stopped with invalid_output_schema before its first model call.
it('compiles the 2020-12 output schemas the core sends', () => {
  const validate = compileOutputSchema({
    $schema: 'https://json-schema.org/draft/2020-12/schema',
    type: 'object',
    properties: {
      version: { type: 'string' },
      error_rate_pct: { type: 'number' },
      regions: { type: 'array', prefixItems: [{ type: 'string' }], items: false },
    },
    required: ['version', 'error_rate_pct'],
    additionalProperties: false,
  });
  expect(validate({ version: 'v42', error_rate_pct: 0.4, regions: ['us'] })).toBe(true);
  expect(validate({ version: 'v42' })).toBe(false);
  expect(validate({ version: 'v42', error_rate_pct: 0.4, regions: ['us', 'eu'] })).toBe(false);
});
