export const OAUTH_ENDPOINTS = ['authorization_endpoint', 'token_endpoint', 'revocation_endpoint', 'registration_endpoint'] as const;
export const OAUTH_AUTH_METHODS = ['none', 'client_secret_basic', 'client_secret_post'] as const;
export const OAUTH_EXTRA_PARAMETERS = ['prompt', 'access_type', 'login_hint'] as const;
export type OAuthAuthMethod = typeof OAUTH_AUTH_METHODS[number];

function invalid(field: string): never {
  throw new Error(`Invalid OAuth ${field}`);
}

export function hasOAuthControlCharacters(text: string): boolean {
  return [...text].some((character) => character.charCodeAt(0) < 32 || character.charCodeAt(0) === 127);
}

function fieldString(value: unknown, field: string, max: number): string {
  if (typeof value !== 'string' || !value || value.length > max || hasOAuthControlCharacters(value)) return invalid(field);
  return value;
}

export function oauthUrl(value: unknown, field: string, allowQuery = true): string {
  const text = fieldString(value, field, 4096);
  if (text !== text.trim() || text.includes('\\')) return invalid(field);
  const url = new URL(text);
  if (url.protocol !== 'https:' || url.username || url.password || url.hash || text.includes('#') ||
      (!allowQuery && text.includes('?'))) return invalid(field);
  // An issuer and redirect URI are exact identities. Preserve trailing slashes
  // and the full URL the administrator confirmed.
  return text;
}

function scopes(value: unknown): string[] {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.length > 128) return invalid('scopes');
  const list = value.map((scope: unknown) => fieldString(scope, 'scope', 256));
  if (list.some((scope) => !/^[\x21\x23-\x5b\x5d-\x7e]+$/.test(scope))) return invalid('scopes');
  return [...new Set(list)].sort();
}

function parameters(value: unknown): Record<string, string> {
  if (value === undefined) return {};
  if (!value || typeof value !== 'object' || Array.isArray(value)) return invalid('parameters');
  return Object.fromEntries(Object.entries(value).map(([name, entry]) => {
    if (!OAUTH_EXTRA_PARAMETERS.some((allowed) => allowed === name)) return invalid('parameter name');
    return [name, fieldString(entry, name, 256)];
  }));
}

export function oauthBinding(config: Record<string, unknown>, groups: 'all' | string[]) {
  const mode = config.registration_mode;
  if (mode !== 'dynamic' && mode !== 'pre-registered') return invalid('registration mode');
  const client = config.client_id;
  const method = config.token_endpoint_auth_method;
  const discovered = config.issuer !== undefined;
  if (method !== undefined && !OAUTH_AUTH_METHODS.some((allowed) => allowed === method)) return invalid('client authentication');
  if (mode === 'pre-registered' && (!client || !method)) return invalid('pre-registered client');
  if (discovered && (typeof config.iss_supported !== 'boolean' || !method)) return invalid('issuer metadata');
  const endpoints = discovered ? {
    resource: oauthUrl(config.resource, 'resource'),
    issuer: oauthUrl(config.issuer, 'issuer', false),
    iss_supported: config.iss_supported as boolean,
    authorization_endpoint: oauthUrl(config.authorization_endpoint, 'authorization endpoint'),
    token_endpoint: oauthUrl(config.token_endpoint, 'token endpoint'),
    revocation_endpoint: config.revocation_endpoint == null ? null : oauthUrl(config.revocation_endpoint, 'revocation endpoint'),
    registration_endpoint: config.registration_endpoint == null ? null : oauthUrl(config.registration_endpoint, 'registration endpoint'),
    redirect_uri: oauthUrl(config.redirect_uri, 'redirect URI', false),
  } : {};
  return {
    url: oauthUrl(config.url, 'MCP URL'),
    registration_mode: mode,
    client_id: client === undefined ? null : fieldString(client, 'client ID', 1024),
    token_endpoint_auth_method: (method ?? null) as OAuthAuthMethod | null,
    scopes: scopes(config.scopes),
    extra_parameters: parameters(config.extra_parameters),
    ...endpoints,
    groups,
  };
}

/** Build or verify the exact browser destination after confirming the binding. */
export function oauthAuthorizationUrl(binding: Record<string, unknown>, state: string, challenge: string): string {
  if (!/^[A-Za-z0-9_-]{43}$/.test(state) || !/^[A-Za-z0-9_-]{43}$/.test(challenge)) return invalid('challenge');
  const authorization = new URL(oauthUrl(binding.authorization_endpoint, 'authorization endpoint'));
  const extra = parameters(binding.extra_parameters);
  for (const name of OAUTH_EXTRA_PARAMETERS) {
    if (extra[name] !== undefined) authorization.searchParams.set(name, extra[name]);
  }
  const params = {
    response_type: 'code',
    client_id: fieldString(binding.client_id, 'client ID', 1024),
    redirect_uri: oauthUrl(binding.redirect_uri, 'redirect URI', false),
    resource: oauthUrl(binding.resource, 'resource'),
    state,
    code_challenge: challenge,
    code_challenge_method: 'S256',
  };
  for (const [key, value] of Object.entries(params)) authorization.searchParams.set(key, value);
  const scope = scopes(binding.scopes).join(' ');
  if (scope) authorization.searchParams.set('scope', scope);
  else authorization.searchParams.delete('scope');
  return authorization.href;
}
