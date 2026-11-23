import { http, HttpResponse } from 'msw';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { server } from '../test/msw/server';
import { ACTION_HEADER, ApiError, createApiRuntime } from './client';
import { registerStepUpHandler, requestStepUp, stepUpRequestFrom } from './step-up';

const stepUp = {
  type: 'urn:runbook:problem:auth.mfa_step_up',
  title: 'auth.mfa_step_up',
  status: 403,
  code: 'auth.mfa_step_up',
  fresh_minutes: 10,
};

function refuseOnce(path: string, body: Record<string, unknown> = stepUp) {
  const seen: { key: string | null; body: string }[] = [];
  server.use(
    http.post(path, async ({ request }) => {
      seen.push({ key: request.headers.get('idempotency-key'), body: await request.text() });
      if (seen.length === 1) {
        return HttpResponse.json(body, {
          status: 403,
          headers: { 'Content-Type': 'application/problem+json' },
        });
      }
      return HttpResponse.json({ id: 'tok_1', secret: 'trb_pat_x' }, { status: 201 });
    }),
  );
  return seen;
}

let unregister: (() => void) | null = null;
afterEach(() => {
  unregister?.();
  unregister = null;
});

beforeEach(() => {
  server.use(http.get('*/v1/me', () => HttpResponse.json({
    user: { id: 'usr_original' }, org: { id: 'org_1' },
  })));
});

describe('step-up retry in the API client', () => {
  it('asks for a second factor, then repeats the request with the same idempotency key and body', async () => {
    const seen = refuseOnce('*/v1/tokens');
    const onStepUp = vi.fn(async () => true);
    const runtime = createApiRuntime({ getToken: () => null, onStepUp });
    const action = 'create-token';
    const { data } = await runtime.client.POST('/v1/tokens', {
      params: { header: { 'Idempotency-Key': runtime.idempotencyKeyFor(action) } },
      body: { kind: 'personal', name: 'laptop', scopes: ['runbooks:read'] } as never,
      headers: { [ACTION_HEADER]: action },
    });
    expect(onStepUp).toHaveBeenCalledTimes(1);
    const [error, request] = onStepUp.mock.calls[0] as unknown as [ApiError, { method: string }];
    expect(error.code).toBe('auth.mfa_step_up');
    expect(error.body.fresh_minutes).toBe(10);
    expect(request.method).toBe('POST');
    expect(seen).toHaveLength(2);
    expect(seen[1]!.key).toBe(seen[0]!.key);
    expect(seen[1]!.body).toBe(seen[0]!.body);
    expect(data).toMatchObject({ id: 'tok_1' });
  });

  it('discards a replay if the signed-in user changes while the prompt is open', async () => {
    const seen = refuseOnce('*/v1/tokens');
    let userId = 'usr_original';
    server.use(http.get('*/v1/me', () => HttpResponse.json({
      user: { id: userId }, org: { id: 'org_1' },
    })));
    const onStepUp = vi.fn(async () => {
      userId = 'usr_other';
      return true;
    });
    const runtime = createApiRuntime({ getToken: () => null, onStepUp });
    await expect(runtime.client.POST('/v1/tokens', {
      params: { header: { 'Idempotency-Key': 'k1' } },
      body: { kind: 'personal' } as never,
    })).rejects.toMatchObject({ code: 'auth.mfa_step_up' });
    expect(onStepUp).toHaveBeenCalledOnce();
    expect(seen).toHaveLength(1);
  });

  it('surfaces the refusal when the person cancels the prompt', async () => {
    const seen = refuseOnce('*/v1/tokens');
    const runtime = createApiRuntime({ getToken: () => null, onStepUp: async () => false });
    const failure = await runtime.client
      .POST('/v1/tokens', {
        params: { header: { 'Idempotency-Key': 'k1' } },
        body: { kind: 'personal' } as never,
      })
      .catch((e: unknown) => e);
    expect(failure).toBeInstanceOf(ApiError);
    expect((failure as ApiError).code).toBe('auth.mfa_step_up');
    expect(seen).toHaveLength(1);
  });

  it('retries only once, then reports a second refusal', async () => {
    let calls = 0;
    server.use(
      http.post('*/v1/tokens', () => {
        calls += 1;
        return HttpResponse.json(stepUp, {
        status: 403, headers: { 'Content-Type': 'application/problem+json' },
      });
      }),
    );
    const runtime = createApiRuntime({ getToken: () => null, onStepUp: async () => true });
    const failure = await runtime.client
      .POST('/v1/tokens', {
        params: { header: { 'Idempotency-Key': 'k1' } },
        body: { kind: 'personal' } as never,
      })
      .catch((e: unknown) => e);
    expect(calls).toBe(2);
    expect((failure as ApiError).code).toBe('auth.mfa_step_up');
  });

  it('never prompts for a code-only refusal or a different API origin', async () => {
    const onStepUp = vi.fn(async () => false);
    for (const [baseUrl, body] of [
      [location.origin, { code: 'auth.mfa_step_up' }],
      ['https://untrusted.example.test', stepUp],
    ] as const) {
      const seen = refuseOnce(`${baseUrl}/v1/tokens`, body);
      const runtime = createApiRuntime({ baseUrl, getToken: () => null, onStepUp });
      await expect(runtime.client.POST('/v1/tokens', {
        params: { header: { 'Idempotency-Key': 'k1' } },
        body: { kind: 'personal' } as never,
      })).rejects.toMatchObject({ code: 'auth.mfa_step_up' });
      expect(seen).toHaveLength(1);
    }
    expect(onStepUp).not.toHaveBeenCalled();
  });

  it('never treats an event-stream response as an MFA problem', async () => {
    server.use(http.post('*/v1/tokens', () => new HttpResponse(JSON.stringify(stepUp), {
      status: 403, headers: { 'Content-Type': 'text/event-stream' },
    })));
    const onStepUp = vi.fn(async () => false);
    const runtime = createApiRuntime({ getToken: () => null, onStepUp });
    await expect(runtime.client.POST('/v1/tokens', {
      params: { header: { 'Idempotency-Key': 'k1' } },
      body: { kind: 'personal' } as never,
    })).rejects.toMatchObject({ status: 403 });
    expect(onStepUp).not.toHaveBeenCalled();
  });

  it('leaves other 403s alone', async () => {
    server.use(
      http.post('*/v1/tokens', () =>
        HttpResponse.json({ status: 403, code: 'auth.forbidden' }, { status: 403 }),
      ),
    );
    const onStepUp = vi.fn(async () => true);
    const runtime = createApiRuntime({ getToken: () => null, onStepUp });
    const failure = await runtime.client
      .POST('/v1/tokens', {
        params: { header: { 'Idempotency-Key': 'k1' } },
        body: { kind: 'personal' } as never,
      })
      .catch((e: unknown) => e);
    expect((failure as ApiError).code).toBe('auth.forbidden');
    expect(onStepUp).not.toHaveBeenCalled();
  });
});

describe('requestStepUp', () => {
  it('resolves false with no prompt registered', async () => {
    await expect(requestStepUp({ code: 'auth.mfa_step_up' }, { method: 'POST' })).resolves.toBe(
      false,
    );
  });

  it('shares one prompt between refusals that arrive together', async () => {
    let resolve: (value: boolean) => void = () => {};
    const handler = vi.fn(
      () =>
        new Promise<boolean>((r) => {
          resolve = r;
        }),
    );
    unregister = registerStepUpHandler(handler);
    const first = requestStepUp({ code: 'auth.mfa_step_up', body: stepUp }, { method: 'post' });
    const second = requestStepUp({ code: 'auth.mfa_step_up', body: stepUp }, { method: 'put' });
    resolve(true);
    await expect(Promise.all([first, second])).resolves.toEqual([true, true]);
    expect(handler).toHaveBeenCalledTimes(1);
    expect(handler).toHaveBeenCalledWith(
      expect.objectContaining({ code: 'auth.mfa_step_up', method: 'POST', freshMinutes: 10 }),
    );
  });

  it('reads the partial session view from an auth.mfa_required body', () => {
    const request = stepUpRequestFrom(
      'auth.mfa_required',
      { security: { mfa_enrolled: false, mfa_partial: true } },
      'get',
    );
    expect(request).toEqual({
      code: 'auth.mfa_required',
      method: 'GET',
      freshMinutes: null,
      security: { mfa_enrolled: false, mfa_partial: true },
    });
  });
});
