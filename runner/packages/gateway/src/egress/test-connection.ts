import type { GatewayTestRequest, GatewayTestResponse, MaterialPush } from '@runbook/runner-shared/contracts/internal';
import { HttpError, type Route } from '@runbook/runner-shared/http';
import { materialValues, type MaterialStore } from '../material.ts';
import { pinnedFetch } from './fetch.ts';
import { connectUpstream, UpstreamAccessBlocked } from '../mcp/upstream.ts';
import { injectHeader } from './inject.ts';

function requestFor(release: MaterialPush, request: GatewayTestRequest['request']) {
  const url = new URL(request.url);
  const binding = release.binding;
  const method = request.method;
  if (!['GET', 'HEAD', 'POST'].includes(method) || url.username || url.password || url.hash) throw new Error('invalid destination');
  if (release.delivery === 'mcp') {
    if (method !== 'POST' || request.url !== binding.url || url.protocol !== 'https:') throw new Error('invalid destination');
  } else if (release.delivery === 'model') {
    const path = binding.provider === 'anthropic' ? '/v1/models' : '/models';
    if (method !== 'GET' || request.url !== `${binding.endpoint}${path}` ||
        (url.protocol !== 'https:' && !(binding.provider === 'openai-compatible' && url.protocol === 'http:' &&
          process.env.RUNNER_ALLOW_HTTP_MODEL_ENDPOINTS === 'true'))) {
      throw new Error('invalid destination');
    }
  } else if (release.delivery === 'proxy') {
    const host = Array.isArray(binding.hosts) ? binding.hosts[0] : undefined;
    const test = binding.test as { method?: string; path?: string } | undefined;
    if (!host || !test || method !== test.method || request.url !== `https://${host}${test.path}`) throw new Error('invalid destination');
  } else {
    throw new Error('invalid destination');
  }
  return url;
}

export function testConnectionRoute(store: MaterialStore, send = pinnedFetch): Route {
  return {
    method: 'POST', path: '/internal/test', callers: ['vault'],
    async handle({ body }) {
      const input = body as GatewayTestRequest | undefined;
      const release = input && typeof input.release_id === 'string' ? store.get(input.release_id) : undefined;
      if (!release || release.step_run_id !== `test:${release.release_id}` || !input?.request) {
        throw new HttpError(404, 'material.unavailable', 'test release unavailable');
      }
      let result: GatewayTestResponse;
      try {
        const request = input.request;
        const url = requestFor(release, request);
        const oauth = release.material.mcp_oauth;
        if (oauth) {
          const upstream = await connectUpstream(url.href, 'streamable-http', async () => ({
            headers: { authorization: `Bearer ${oauth.access_token}` },
            oauth: { release_id: release.release_id, token_version: oauth.token_version },
            signal: store.signal(release),
          }), AbortSignal.timeout(10_000));
          try {
            await upstream.list();
          } finally {
            await upstream.close();
          }
          return { body: { outcome: 'response', status: 200 } satisfies GatewayTestResponse };
        }
        const headers = new Headers();
        for (const name of ['accept', 'content-type', 'anthropic-version']) {
          const value = request.headers?.[name];
          if (typeof value === 'string') headers.set(name, value);
        }
        const fields = materialValues(release.material);
        if (fields.length !== 1 || !fields[0]) throw new Error('test material unavailable');
        injectHeader(headers, release.binding, fields[0]);
        const response = await send(url, {
          headers, method: request.method,
          ...(request.method === 'POST' && typeof request.body === 'string' && { body: request.body }),
          signal: AbortSignal.timeout(10_000), redirect: 'error',
        });
        await response.body?.cancel();
        result = { outcome: 'response', status: response.status };
      } catch (error) {
        if (error instanceof UpstreamAccessBlocked) {
          return { body: { outcome: 'response', status: (error.data as { kind?: string })?.kind === 'insufficient_scope' ? 403 : 401 } };
        }
        const code = (error as { cause?: { code?: unknown } }).cause?.code;
        const tls = typeof code === 'string' && /^(ERR_TLS_|CERT_|UNABLE_TO_VERIFY_|DEPTH_ZERO_)/.test(code);
        result = { outcome: tls ? 'tls' : 'unreachable' };
      }
      return { body: result };
    },
  };
}
