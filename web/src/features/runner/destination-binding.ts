import { oauthBinding } from '../../../../runner/packages/shared/src/oauth-binding.ts';
import { secretManagerReference } from '../../../../runner/packages/shared/src/credential-source.ts';
import { toBase64Url } from './encoding.ts';

export interface DestinationConfig {
  type: string;
  config: Record<string, unknown>;
  groups: unknown;
}

const bearer = { header_name: 'authorization', header_format: 'Bearer {}' };
const providerHeaders = {
  anthropic: { header_name: 'x-api-key', header_format: '{}' },
  openai: bearer,
  'vertex-gemini': bearer,
  google: { header_name: 'x-goog-api-key', header_format: '{}' },
  'azure-openai': { header_name: 'api-key', header_format: '{}' },
  'openai-compatible': bearer,
};

function invalid(): never {
  throw new Error('Invalid credential destination');
}

function text(value: unknown): string {
  return typeof value === 'string' && value.length > 0 ? value : invalid();
}

function sortedUnique(values: unknown): string[] {
  if (!Array.isArray(values)) return invalid();
  return [...new Set(values.map(text))].sort();
}

function endpoint(value: unknown, allowHttp = false): string {
  const url = new URL(text(value).trim());
  if (
    (url.protocol !== 'https:' && !(allowHttp && url.protocol === 'http:')) ||
    url.username ||
    url.password ||
    url.search ||
    url.hash
  )
    return invalid();
  return url.origin + url.pathname.replace(/\/+$/, '');
}

export function destinationBinding(input: DestinationConfig): Record<string, unknown> {
  const { type, config } = input;
  const groups = input.groups === 'all' ? 'all' : sortedUnique(input.groups);
  const source =
    config.secret_source === undefined
      ? {}
      : { secret_source: secretManagerReference(config.secret_source) };
  switch (type) {
    case 'api-key': {
      if (!Array.isArray(config.hosts) || !config.hosts.length) return invalid();
      const hosts = sortedUnique(
        config.hosts.map((value) => text(value).trim().toLowerCase().replace(/\.$/, '')),
      );
      const header_name = text(config.header_name).toLowerCase();
      const header_format = text(config.header_format);
      const test = config.test as { method?: unknown; path?: unknown } | undefined;
      const method = test?.method ?? 'GET';
      const path = test?.path ?? '/';
      if (method !== 'GET' && method !== 'HEAD') return invalid();
      return {
        hosts,
        header_name,
        header_format,
        test: { method, path: text(path) },
        groups,
        ...source,
      };
    }
    case 'model-provider': {
      const provider = text(config.provider);
      if (!Object.hasOwn(providerHeaders, provider)) return invalid();
      const headers = providerHeaders[provider as keyof typeof providerHeaders];
      return {
        provider,
        endpoint: endpoint(config.endpoint, provider === 'openai-compatible'),
        ...headers,
        groups,
        ...source,
      };
    }
    case 'mcp-bearer':
      return { url: endpoint(config.url), ...bearer, groups, ...source };
    case 'mcp-oauth':
      return oauthBinding(config, groups);
    default:
      return invalid();
  }
}

// RFC 8785 member order for binding objects. The supported binding data is
// strings, arrays of strings and objects, so no numeric normalization is needed.
export function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  if (typeof value === 'object' && value !== null) {
    const object = value as Record<string, unknown>;
    return `{${Object.keys(object)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(object[key])}`)
      .join(',')}}`;
  }
  return JSON.stringify(value);
}

export async function credentialBindingHash(input: DestinationConfig): Promise<string> {
  const bytes = new TextEncoder().encode(
    canonicalJson({ type: input.type, binding: destinationBinding(input) }),
  );
  return toBase64Url(new Uint8Array(await crypto.subtle.digest('SHA-256', bytes)));
}
