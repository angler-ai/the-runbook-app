import { boundedProviderFetch } from './stream-fetch.ts';
import { geminiModel } from './gemini.ts';
import { vertexBinding } from '@runbook/runner-shared/vertex-binding';
import { pinnedFetch } from '../egress/fetch.ts';
import { createVertexAnthropic } from '@ai-sdk/google-vertex/anthropic';
import { createAnthropic } from '@ai-sdk/anthropic';
import { createOpenAICompatible } from '@ai-sdk/openai-compatible';
import { generateText, type ModelMessage } from 'ai';
import type { ModelProfile } from './endpoint.ts';
import type { ModelRequest } from './pricing.ts';

export function anthropicPrompt(prompt: string): ModelMessage[] {
  return [{ role: 'user', content: prompt, providerOptions: { anthropic: { cacheControl: { type: 'ephemeral' } } } }];
}

export function providerModel(profile: ModelProfile, fields: Record<string, string>, wrapFetch = (fetcher: typeof fetch) => fetcher, dispatched?: () => void) {
  if (profile.provider === 'bedrock') throw new Error('attached model identity is unavailable');
  const endpoint = new URL(profile.endpoint);
  if ((endpoint.protocol !== 'https:' && !(profile.provider === 'openai-compatible' && endpoint.protocol === 'http:' &&
      process.env.RUNNER_ALLOW_HTTP_MODEL_ENDPOINTS === 'true')) ||
      endpoint.username || endpoint.password || endpoint.search || endpoint.hash) throw new Error('invalid model endpoint');
  const attached = profile.vertexBinding;
  if (attached && (vertexBinding(profile.provider, attached.project_id, attached.region)?.endpoint !== profile.endpoint ||
      attached.endpoint !== profile.endpoint)) throw new Error('invalid Vertex model endpoint');
  const key = profile.provider === 'vertex' || attached ? fields.access_token : fields.api_key;
  if (!key) throw new Error('model credential unavailable');
  const guardedFetch: typeof fetch = async (input, init) => {
    const url = new URL(input instanceof Request ? input.url : input);
    if (url.origin !== endpoint.origin) throw new Error('invalid model destination');
    if (attached) {
      const modelPath = `${endpoint.pathname}/${profile.model}:`;
      const methods = profile.provider === 'vertex' ? ['rawPredict', 'streamRawPredict'] : ['generateContent', 'streamGenerateContent'];
      if (!methods.some((method) => url.pathname === modelPath + method)) throw new Error('invalid Vertex model destination');
    }
    const headers = new Headers(init?.headers);
    return pinnedFetch(url, { ...init, headers, redirect: 'error' }, dispatched);
  };
  const options = { baseURL: profile.provider === 'anthropic' ? `${profile.endpoint}/v1` : profile.endpoint, fetch: wrapFetch(guardedFetch) };
  if (profile.provider === 'vertex-gemini') return geminiModel(profile.model, endpoint, key, options.fetch, !attached);
  if (profile.provider === 'vertex') {
    if (!/^\/v1\/projects\/[\w-]+\/locations\/[\w-]+\/publishers\/anthropic\/models\/?$/.test(endpoint.pathname) ||
        !/^(?:[a-z0-9-]+-)?aiplatform(?:\.(?:eu|us)\.rep)?\.googleapis\.com$/.test(endpoint.hostname) || endpoint.port ||
        !/^claude-[\w-]+(?:@\d{8})?$/.test(profile.model)) throw new Error('invalid Vertex model endpoint');
    return createVertexAnthropic({ ...options, generateAuthToken: async () => key,
      fetch: async (input, init) => {
        const headers = new Headers(init?.headers);
        // Current partner Messages API accepts plain mid-conversation system messages.
        // The installed SDK still attaches the original preview header automatically.
        const beta = headers.get('anthropic-beta')?.split(',').map((name) => name.trim())
          .filter((name) => name !== 'mid-conversation-system-2026-04-07');
        if (beta?.length) headers.set('anthropic-beta', beta.join(','));
        else headers.delete('anthropic-beta');
        return options.fetch(input, { ...init, headers });
      },
    })(profile.model);
  }
  return profile.provider === 'anthropic'
    ? createAnthropic({ ...options, apiKey: key })(profile.model)
    : createOpenAICompatible({ ...options, name: 'runbook', apiKey: key })(profile.model);
}

export async function invokeModel(profile: ModelProfile, request: ModelRequest, fields: Record<string, string>, signal?: AbortSignal,
  dispatched?: () => void, respond?: (status: number) => void) {
  const model = providerModel(profile, fields, (fetcher) => boundedProviderFetch(fetcher, respond), dispatched);
  const system = request.messages?.filter(({ role }) => role === 'system').map(({ content }) => content).join('\n\n');
  const messages = request.messages?.flatMap(({ role, content }): ModelMessage[] =>
    role === 'user' || role === 'assistant' ? [{ role, content }] : []);
  const result = await generateText({
    model, maxOutputTokens: request.max_tokens, maxRetries: 0,
    ...(system && { system }), ...(signal && { abortSignal: signal }),
    ...(profile.provider === 'vertex-gemini' && { providerOptions: { googleVertex: { thinkingConfig: { thinkingLevel: 'low' } } } }),
    messages: messages ?? ((profile.provider === 'anthropic' || profile.provider === 'vertex') ? anthropicPrompt(request.prompt) : [{ role: 'user', content: request.prompt }]),
  });
  const { inputTokens, outputTokens, inputTokenDetails } = result.usage;
  if (inputTokens === undefined || outputTokens === undefined) throw new Error('model usage unavailable');
  return { text: result.text, inputTokens, outputTokens,
    cacheReadTokens: inputTokenDetails.cacheReadTokens, cacheWriteTokens: inputTokenDetails.cacheWriteTokens };
}
