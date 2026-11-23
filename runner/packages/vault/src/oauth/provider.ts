import { ProxyAgent, fetch as proxyFetch } from 'undici';
import { isCredentialPlaceholder, isVisibleAsciiToken } from '@runbook/runner-shared/credential-source';
import { findSecretShapedValues } from '@runbook/runner-shared/secret-shape';
import { hasOAuthControlCharacters, oauthBinding, oauthUrl, OAUTH_AUTH_METHODS, OAUTH_ENDPOINTS, type OAuthAuthMethod } from '@runbook/runner-shared/oauth-binding';
import { VaultError } from '../errors.ts';
import { record } from '../types/common.ts';

const maxBody = 128 * 1024;
const timeout = 12_000;

export class ProviderRequestError extends VaultError {
  readonly sent: boolean;
  readonly providerStatus?: number;

  constructor(sent: boolean, providerStatus?: number) {
    super(502, 'oauth.provider_unavailable', 'the OAuth endpoint did not return a usable response');
    this.sent = sent;
    this.providerStatus = providerStatus;
  }
}

export interface ProviderResponse {
  status: number;
  headers: { get(name: string): string | null };
  body: unknown;
}

export interface OAuthProvider {
  check(url: string): Promise<void>;
  request(url: string, input?: { method: 'POST'; body: string; headers: Record<string, string> }, readBody?: boolean): Promise<ProviderResponse>;
}

function authority(url: string): string {
  const parsed = new URL(oauthUrl(url, 'destination'));
  if (parsed.port) throw new VaultError(400, 'oauth.destination_denied', 'OAuth endpoints must use HTTPS on port 443');
  return `${parsed.hostname}:443`;
}

export class TunneledOAuthProvider implements OAuthProvider {
  private readonly gateway: string;
  private readonly token: string;

  constructor(gateway: string, token: string) {
    this.gateway = gateway;
    this.token = token;
  }

  async check(url: string): Promise<void> {
    const response = await fetch(`${this.gateway}/internal/oauth/destination`, {
      method: 'POST', redirect: 'error', signal: AbortSignal.timeout(timeout),
      headers: { authorization: `Bearer ${this.token}`, 'content-type': 'application/json' },
      body: JSON.stringify({ authority: authority(url) }),
    }).catch(() => undefined);
    await response?.body?.cancel();
    if (!response?.ok) throw new VaultError(400, 'oauth.destination_denied', 'OAuth endpoint is not reachable under egress policy');
  }

  async request(url: string, input?: { method: 'POST'; body: string; headers: Record<string, string> }, readBody = true): Promise<ProviderResponse> {
    authority(url);
    let sent = false;
    const agent = new ProxyAgent({ uri: this.gateway, token: `Bearer ${this.token}` });
    // This handler belongs to the HTTPS request, after CONNECT and TLS.
    // From this point on, the provider may have processed the body.
    const dispatcher = agent.compose((dispatch) => (options, handler) => dispatch(options, {
      onResponseStart: handler.onResponseStart?.bind(handler),
      onResponseData: handler.onResponseData?.bind(handler),
      onResponseEnd: handler.onResponseEnd?.bind(handler),
      onResponseError: handler.onResponseError?.bind(handler),
      onRequestUpgrade: handler.onRequestUpgrade?.bind(handler),
      onRequestStart(controller, context) {
        sent = true;
        handler.onRequestStart?.(controller, context);
      },
    }));
    let upstreamStatus: number | undefined;
    try {
      const response = await proxyFetch(url, {
        ...input, dispatcher, redirect: 'error', signal: AbortSignal.timeout(timeout),
      });
      upstreamStatus = response.status;
      let body: unknown = null;
      if (response.body && readBody) {
        const reader = response.body.getReader();
        const chunks: Buffer[] = [];
        let bytes = 0;
        while (true) {
          const part = await reader.read();
          if (part.done) break;
          bytes += part.value.byteLength;
          if (bytes > maxBody) throw new Error('OAuth response too large');
          chunks.push(Buffer.from(part.value));
        }
        if (bytes) {
          try {
            body = JSON.parse(Buffer.concat(chunks).toString('utf8')) as unknown;
          } catch {
            // A non-JSON response is never valid provider metadata or token material.
          }
        }
      } else {
        await response.body?.cancel();
      }
      return { status: response.status, headers: response.headers, body };
    } catch {
      if (upstreamStatus !== undefined && (upstreamStatus >= 500 || upstreamStatus === 429)) {
        return { status: upstreamStatus, headers: new Headers(), body: null };
      }
      throw new ProviderRequestError(sent, upstreamStatus);
    } finally {
      await agent.destroy();
    }
  }
}

const fields = (value: unknown) => value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};
const strings = (value: unknown) => Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string') : [];
const challengeValue = (header: string, name: 'resource_metadata' | 'scope') =>
  new RegExp(`(?:^|[\\s,])${name}="([^"\\r\\n]+)"`, 'i').exec(header)?.[1];

function invalidMetadata(): never {
  throw new VaultError(422, 'oauth.invalid_metadata', 'the provider did not advertise compatible OAuth metadata');
}

async function metadata(provider: OAuthProvider, urls: string[]) {
  for (const url of new Set(urls)) {
    const response = await provider.request(url);
    if (response.status >= 200 && response.status < 300) {
      if (!response.body || !Object.keys(fields(response.body)).length) invalidMetadata();
      return fields(response.body);
    }
  }
  return invalidMetadata();
}

function wellKnown(url: URL, suffix: string): string {
  return `${url.origin}/.well-known/${suffix}${url.pathname === '/' ? '' : url.pathname.replace(/\/$/, '')}`;
}

export async function discoverOAuth(provider: OAuthProvider, config: Record<string, unknown>) {
  const input = oauthBinding(config, 'all');
  const server = new URL(input.url);
  const probe = await provider.request(input.url, {
    method: 'POST',
    headers: { 'content-type': 'application/json', accept: 'application/json, text/event-stream' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: {
      protocolVersion: '2025-11-25', capabilities: {}, clientInfo: { name: 'runbook', version: '1' },
    } }),
  }, false);
  const challenge = probe.status === 401 ? probe.headers.get('www-authenticate') ?? '' : '';
  const advertised = challengeValue(challenge, 'resource_metadata');
  const resourceMetadata = await metadata(provider, advertised ?
    [oauthUrl(advertised, 'resource metadata')] :
    [wellKnown(server, 'oauth-protected-resource'), `${server.origin}/.well-known/oauth-protected-resource`]);

  const resourceId = oauthUrl(resourceMetadata.resource, 'resource');
  const resource = new URL(resourceId);
  const prefix = resource.pathname.endsWith('/') ? resource.pathname : `${resource.pathname}/`;
  if (resource.origin !== server.origin ||
      (resource.pathname !== server.pathname && (!server.pathname.startsWith(prefix) || /[%\\;]|\/\//.test(resource.pathname))) ||
      (resource.search && resource.search !== server.search)) invalidMetadata();

  const issuer = oauthUrl(strings(resourceMetadata.authorization_servers)[0], 'issuer', false);
  const parsed = new URL(issuer);
  const authorization = await metadata(provider, [
    wellKnown(parsed, 'oauth-authorization-server'),
    wellKnown(parsed, 'openid-configuration'),
    `${parsed.origin}${parsed.pathname.replace(/\/$/, '')}/.well-known/openid-configuration`,
  ]);
  if (authorization.issuer !== issuer || !strings(authorization.code_challenge_methods_supported).includes('S256')) invalidMetadata();
  const methods = strings(authorization.token_endpoint_auth_methods_supported);
  const method = input.token_endpoint_auth_method ??
    (methods.length ? OAUTH_AUTH_METHODS.find((entry) => methods.includes(entry)) : 'none');
  if (!method || (methods.length > 0 && !methods.includes(method))) invalidMetadata();
  const endpoints = Object.fromEntries(OAUTH_ENDPOINTS.map((endpoint) => {
    const value = authorization[endpoint];
    const required = endpoint === 'authorization_endpoint' || endpoint === 'token_endpoint' ||
      (endpoint === 'registration_endpoint' && input.registration_mode === 'dynamic');
    return [endpoint, !required && value == null ? null : oauthUrl(value, endpoint)];
  }));
  if (findSecretShapedValues(endpoints).length) invalidMetadata();
  await Promise.all(Object.values(endpoints).flatMap((url) => url === null ? [] : [provider.check(url)]));
  const challengedScope = challengeValue(challenge, 'scope');
  return {
    ...config,
    ...endpoints,
    resource: resourceId,
    issuer,
    iss_supported: authorization.authorization_response_iss_parameter_supported === true,
    token_endpoint_auth_method: method,
    scopes: input.scopes.length ? input.scopes : challengedScope?.split(/ +/) ?? strings(resourceMetadata.scopes_supported),
  };
}

export async function registerOAuth(provider: OAuthProvider, config: Record<string, unknown>) {
  const response = await provider.request(oauthUrl(config.registration_endpoint, 'registration endpoint'), {
    method: 'POST', headers: { 'content-type': 'application/json', accept: 'application/json' },
    body: JSON.stringify({
      client_name: 'The Runbook', redirect_uris: [config.redirect_uri], grant_types: ['authorization_code', 'refresh_token'],
      response_types: ['code'], token_endpoint_auth_method: config.token_endpoint_auth_method,
    }),
  });
  const body = fields(response.body);
  if ((response.status >= 400 && response.status < 500)
      || (response.status === 503 && body.error === 'temporarily_unavailable')) {
    throw new VaultError(502, 'oauth.registration_refused', 'the OAuth provider declined client registration');
  }
  const clientId = body.client_id;
  const secret = body.client_secret === undefined ? null : token(body.client_secret, true);
  const expiration = body.client_secret_expires_at ?? 0;
  if (response.status < 200 || response.status >= 300 || typeof clientId !== 'string' ||
      !clientId || clientId.length > 1024 || (body.token_endpoint_auth_method !== undefined &&
      body.token_endpoint_auth_method !== config.token_endpoint_auth_method) ||
      (config.token_endpoint_auth_method !== 'none' && !secret) ||
      (secret !== null && clientId.includes(secret)) ||
      typeof expiration !== 'number' || !Number.isSafeInteger(expiration) || expiration < 0) {
    throw new VaultError(502, 'oauth.registration_failed', 'OAuth client registration did not return a compatible client');
  }
  return { clientId, secret: config.token_endpoint_auth_method === 'none' ? null : secret,
    secretExpiresAt: expiration > 0 ? new Date(Math.min(expiration, 8_640_000_000_000) * 1000) : null };
}

function token(value: unknown, opaque = false): string {
  if (typeof value !== 'string' || !value || value.length > 16_384 ||
      (opaque ? hasOAuthControlCharacters(value) : !isVisibleAsciiToken(value)) || isCredentialPlaceholder(value)) {
    throw new VaultError(502, 'oauth.invalid_token_response', 'the OAuth provider returned an invalid token response');
  }
  return value;
}

export function tokenPair(response: ProviderResponse, now: Date, fallbackScopes: string[],
  clientSecret?: string, submittedGrant?: string, refreshing = false) {
  const body = fields(response.body);
  if (response.status < 200 || response.status >= 300) {
    if (refreshing && (body.error === 'invalid_client' || body.error === 'unauthorized_client')) {
      throw new VaultError(409, `oauth.${body.error}`, 'the OAuth provider refused client authentication');
    }
    if (response.status >= 500 || response.status === 429 ||
        (refreshing && response.status >= 400 && response.status < 500 && typeof body.error !== 'string')) {
      throw new VaultError(503, 'oauth.refresh_retry', 'the OAuth provider is temporarily unavailable');
    }
    const invalidGrant = ['invalid_grant', 'unauthorized_client', 'invalid_token'].includes(String(body.error));
    const code = invalidGrant ? 'oauth.invalid_grant'
      : typeof body.error === 'string' ? 'oauth.refresh_retry' : 'oauth.exchange_failed';
    throw new VaultError(invalidGrant ? 409 : 502, code, 'the OAuth token request failed');
  }
  if (typeof body.token_type !== 'string' || body.token_type.toLowerCase() !== 'bearer') {
    throw new VaultError(502, 'oauth.invalid_token_response', 'the OAuth provider did not return a bearer token');
  }
  const seconds = body.expires_in ?? 300;
  if (typeof seconds !== 'number' || !Number.isFinite(seconds) || seconds <= 0) {
    throw new VaultError(502, 'oauth.invalid_token_response', 'the OAuth provider returned an invalid token expiry');
  }
  const access = token(body.access_token);
  const refresh = body.refresh_token === undefined ? null : token(body.refresh_token, true);
  if (access === refresh || access === clientSecret || access === submittedGrant ||
      (refresh && (refresh === clientSecret || (!refreshing && refresh === submittedGrant)))) {
    throw new VaultError(502, 'oauth.invalid_token_response', 'the OAuth provider returned colliding credentials');
  }
  const secrets = [access, refresh, clientSecret, submittedGrant, body.id_token].filter((value): value is string => typeof value === 'string' && value.length > 0);
  const displaySafe = (value: string) => !secrets.some((secret) => value.includes(secret)) &&
    !findSecretShapedValues(value).length;
  const scope = body.scope;
  if (scope !== undefined && (typeof scope !== 'string' || scope.length > 4096 || !displaySafe(scope) || !/^[\x20\x21\x23-\x5b\x5d-\x7e]*$/.test(scope))) {
    throw new VaultError(502, 'oauth.invalid_token_response', 'the OAuth provider returned invalid scopes');
  }
  // Identity here is display metadata from the bound HTTPS token endpoint.
  // It does not authenticate anyone to Runbook or grant any permissions.
  let identity: Record<string, unknown> = {};
  if (typeof body.id_token === 'string' && body.id_token.length < 16_384) {
    const encoded = body.id_token.split('.')[1];
    if (encoded) {
      try {
        identity = fields(JSON.parse(Buffer.from(encoded, 'base64url').toString('utf8')) as unknown);
      } catch {
        // The provider did not return usable display identity.
      }
    }
  }
  const safeText = (value: unknown): value is string =>
    typeof value === 'string' && value.trim().length > 0 && value.length <= 256 &&
    !/[\p{Cf}\p{Cc}\p{Zl}\p{Zp}]/u.test(value) && displaySafe(value);
  const displayName = (value: unknown) => {
    if (typeof value !== 'string') return undefined;
    const name = value.replace(/[\p{Cf}\p{Cc}\p{Zl}\p{Zp}]/gu, '').trim();
    return safeText(name) ? name : undefined;
  };
  const owner = fields(body.owner);
  const user = fields(owner.user);
  const email = [body.email, owner.email, user.email, identity.email].find(safeText);
  const workspaceName = displayName(body.workspace_name);
  const providerName = [body.account_name, body.name, body.connected_account, body.account, identity.name]
    .map(displayName).find((name) => name !== undefined);
  const identifiers = [
    { kind: 'workspace', id: body.workspace_id, name: workspaceName },
    { kind: 'account', id: body.account_id, name: providerName },
    { kind: 'user', id: user.id, name: displayName(user.name) ?? workspaceName },
    { kind: 'user', id: body.user_id, name: displayName(body.user_name) ?? workspaceName },
    { kind: 'subject', id: identity.sub, name: providerName },
    { kind: 'bot', id: body.bot_id, name: workspaceName },
  ];
  const stable = identifiers.find(({ id }) => safeText(id));
  const id = stable && safeText(stable.id) ? stable.id : undefined;
  const name = stable?.name ?? workspaceName ?? providerName ?? displayName(user.name) ?? displayName(body.user_name) ?? email;
  const identifier = id ?? email;
  const label = name && identifier && name !== identifier
    ? `${/[()·]/u.test(name) ? JSON.stringify(name) : name} (${identifier})`
    : identifier ?? name;
  const details = [body.email_domain, stable?.kind !== 'bot' && safeText(body.bot_id) ? `bot ${body.bot_id}` : undefined]
    .filter(safeText);
  if (label && displaySafe(label)) details.unshift(label);
  const account = details.join(' · ') || identifier || name;
  const expiresAt = new Date(now.getTime() + Math.max(1, Math.min(Math.floor(seconds), 86_400)) * 1000);
  return {
    access, refresh,
    expiresAt,
    scopes: scope === undefined ? fallbackScopes : [...new Set(scope.split(' ').filter(Boolean))].sort(),
    account,
    accountIdentity: stable && id ? { kind: stable.kind, id } : undefined,
  };
}

const formEncode = (value: string) => new URLSearchParams({ _: value }).toString().slice(2);

export function formRequest(config: Record<string, unknown>, body: URLSearchParams, clientSecret?: string) {
  const binding = record(config);
  const headers: Record<string, string> = { 'content-type': 'application/x-www-form-urlencoded', accept: 'application/json' };
  const clientId = binding.client_id as string;
  const method = binding.token_endpoint_auth_method as OAuthAuthMethod;
  if (method === 'client_secret_basic' && clientSecret !== undefined) {
    const basic = Buffer.from(`${formEncode(clientId)}:${formEncode(clientSecret)}`).toString('base64');
    headers.authorization = `Basic ${basic}`;
  } else if (method === 'client_secret_post' && clientSecret !== undefined) {
    body.set('client_secret', clientSecret);
  }
  return { method: 'POST' as const, body: body.toString(), headers };
}
