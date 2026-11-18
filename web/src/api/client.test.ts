import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { server } from '../test/msw/server';
import {
  ACTION_HEADER,
  ApiError,
  api,
  createApiRuntime,
  registerUnauthorizedHandler,
} from './client';
import { tokenStore } from './token-store';

function deferred() {
  let resolve: () => void = () => {};
  const promise = new Promise<void>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

const problem = {
  type: 'urn:runbook:problem:execution.inputs_invalid',
  title: 'execution.inputs_invalid',
  status: 422,
  detail: 'inputs do not match the version schema',
  instance: 'urn:request:req_123',
  code: 'execution.inputs_invalid',
  errors: [{ path: 'service', code: 'enum', message: 'must be one of api, web' }],
};

describe('API client middleware', () => {
  it('sends the bearer token and maps problem+json to ApiError', async () => {
    let authorization: string | null = null;
    server.use(
      http.post('*/v1/executions/:exe/rerun', ({ request }) => {
        authorization = request.headers.get('authorization');
        return HttpResponse.json(problem, {
          status: 422,
          headers: { 'Content-Type': 'application/problem+json', 'X-Request-ID': 'req_123' },
        });
      }),
    );
    const { client } = createApiRuntime({ getToken: () => 'trb_pat_test' });
    const failure = await client
      .POST('/v1/executions/{exe}/rerun', {
        params: { path: { exe: 'exe_1' }, header: { 'Idempotency-Key': 'given' } },
        body: {},
      })
      .catch((e: unknown) => e);
    expect(authorization).toBe('Bearer trb_pat_test');
    expect(failure).toBeInstanceOf(ApiError);
    const apiError = failure as ApiError;
    expect(apiError.status).toBe(422);
    expect(apiError.code).toBe('execution.inputs_invalid');
    expect(apiError.requestId).toBe('req_123');
    expect(apiError.errors).toEqual(problem.errors);
  });

  it('reuses one idempotency key per user action across retries (D20)', async () => {
    const keys: (string | null)[] = [];
    server.use(
      http.post('*/v1/validate', ({ request }) => {
        keys.push(request.headers.get('idempotency-key'));
        expect(request.headers.get(ACTION_HEADER)).toBeNull();
        return HttpResponse.json({ diagnostics: [], step_budget: 1 });
      }),
    );
    const runtime = createApiRuntime({ getToken: () => 'trb_pat_test' });
    const call = (action: string) =>
      runtime.client.POST('/v1/validate', {
        body: { source: 'x' },
        headers: { [ACTION_HEADER]: action },
      });
    await call('action-a');
    await call('action-a'); // retry of the same user action
    await call('action-b');
    expect(keys[0]).toBeTruthy();
    expect(keys[1]).toBe(keys[0]);
    expect(keys[2]).not.toBe(keys[0]);
    runtime.forgetAction('action-a');
    await call('action-a');
    expect(keys[3]).not.toBe(keys[0]);
  });

  it('passes If-Match through untouched', async () => {
    let seen: string | null = null;
    server.use(
      http.patch('*/v1/runbooks/:rb/draft', ({ request }) => {
        seen = request.headers.get('if-match');
        return HttpResponse.json({
          runbook_id: 'rb_1',
          source: 'x',
          revision: 2,
          diagnostics: [],
        });
      }),
    );
    const { client } = createApiRuntime({ getToken: () => 'tok' });
    const { error } = await client.PATCH('/v1/runbooks/{rb}/draft', {
      params: { path: { rb: 'rb_1' }, header: { 'If-Match': '"rev-1"' } },
      body: { source: 'x' },
    });
    expect(error).toBeUndefined();
    expect(seen).toBe('"rev-1"');
  });

  it('notifies onUnauthorized exactly once for a 401', async () => {
    server.use(
      http.get('*/v1/me/tasks', () =>
        HttpResponse.json(
          { ...problem, status: 401, code: 'auth.invalid_credential' },
          { status: 401, headers: { 'Content-Type': 'application/problem+json' } },
        ),
      ),
    );
    const onUnauthorized = vi.fn();
    const { client } = createApiRuntime({ getToken: () => 'tok', onUnauthorized });
    await expect(client.GET('/v1/me/tasks')).rejects.toBeInstanceOf(ApiError);
    expect(onUnauthorized).toHaveBeenCalledTimes(1);
  });

  it('tells onUnauthorized which token the refused request carried (U14)', async () => {
    const seen = deferred();
    const release = deferred();
    server.use(
      http.get('*/v1/me/tasks', async () => {
        seen.resolve();
        await release.promise;
        return HttpResponse.json(
          { ...problem, status: 401, code: 'auth.invalid_credential' },
          { status: 401, headers: { 'Content-Type': 'application/problem+json' } },
        );
      }),
    );
    let current = 'tok-old';
    const onUnauthorized = vi.fn();
    const { client } = createApiRuntime({ getToken: () => current, onUnauthorized });
    const pending = client.GET('/v1/me/tasks').catch(() => undefined);
    await seen.promise;
    current = 'tok-new';
    release.resolve();
    await pending;
    expect(onUnauthorized).toHaveBeenCalledWith(expect.any(ApiError), { token: 'tok-old' });
  });
});

describe('app-wide 401 handling (U14)', () => {
  const unauthorized = () =>
    HttpResponse.json(
      { ...problem, status: 401, code: 'auth.invalid_credential' },
      { status: 401, headers: { 'Content-Type': 'application/problem+json' } },
    );

  afterEach(() => {
    registerUnauthorizedHandler(null);
    tokenStore.clear();
  });

  it('a late 401 from the old token leaves a newer session alone', async () => {
    const seen = deferred();
    const release = deferred();
    server.use(
      http.get('*/v1/me/tasks', async () => {
        seen.resolve();
        await release.promise;
        return unauthorized();
      }),
    );
    const handler = vi.fn();
    registerUnauthorizedHandler(handler);
    tokenStore.set('trb_pat_old');
    const pending = api.GET('/v1/me/tasks').catch((e: unknown) => e);
    await seen.promise;
    tokenStore.set('trb_pat_new');
    release.resolve();
    expect(await pending).toBeInstanceOf(ApiError);
    expect(tokenStore.get()).toBe('trb_pat_new');
    expect(handler).not.toHaveBeenCalled();
  });

  it('a 401 for the current token ends the session and hands off to the router', async () => {
    server.use(http.get('*/v1/me/tasks', unauthorized));
    const handler = vi.fn();
    registerUnauthorizedHandler(handler);
    tokenStore.set('trb_pat_current');
    await api.GET('/v1/me/tasks').catch(() => undefined);
    expect(tokenStore.get()).toBeNull();
    expect(handler).toHaveBeenCalledTimes(1);
  });
});
