// `model-provider` (design §7.2, §8.1): a provider API key, added by the
// gateway's model endpoint (`model` delivery). The binding is the provider,
// its endpoint and the auth header the provider uses (derived from the
// provider, so it can't be pointed elsewhere), plus the allowed groups.
import { MODEL_PROVIDERS } from '@runbook/runner-shared/credential-types';
import type { CredentialTypeImpl } from './index.ts';
import { canonicalGroups, type Groups, httpsUrl, invalid, record } from './common.ts';

type Provider = (typeof MODEL_PROVIDERS)[number];

const AUTH: Record<Provider, { header_name: string; header_format: string }> = {
  anthropic: { header_name: 'x-api-key', header_format: '{}' },
  openai: { header_name: 'authorization', header_format: 'Bearer {}' },
  'vertex-gemini': { header_name: 'authorization', header_format: 'Bearer {}' },
  google: { header_name: 'x-goog-api-key', header_format: '{}' },
  'azure-openai': { header_name: 'api-key', header_format: '{}' },
  'openai-compatible': { header_name: 'authorization', header_format: 'Bearer {}' },
};

export const modelProvider: CredentialTypeImpl = {
  type: 'model-provider',
  secretFields: ['api_key'],
  deliveries: ['model'],
  binding(config: unknown, groups: Groups) {
    const c = record(config);
    const provider = c.provider as Provider;
    if (!MODEL_PROVIDERS.includes(provider)) throw invalid(`provider must be one of ${MODEL_PROVIDERS.join(', ')}`);
    // Local OpenAI-compatible servers (Ollama, vLLM) may be plain http; hosted providers use https.
    const endpoint = httpsUrl(c.endpoint, 'endpoint', { allowHttp: provider === 'openai-compatible' && process.env.RUNNER_ALLOW_HTTP_MODEL_ENDPOINTS === 'true' });
    return { provider, endpoint, ...AUTH[provider], groups: canonicalGroups(groups) };
  },
  testRequest(binding) {
    const endpoint = binding.endpoint as string;
    if (binding.provider === 'anthropic') {
      return { method: 'GET', url: `${endpoint}/v1/models`, headers: { 'anthropic-version': '2023-06-01' } };
    }
    return { method: 'GET', url: `${endpoint}/models` };
  },
};
