import { createHash, createPublicKey, verify } from 'node:crypto';
import { createServer } from 'node:http';
import type { AddressInfo } from 'node:net';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { lengthPrefixed } from '@runbook/runner-shared/encoding';
import { bindingHash } from '@runbook/runner-shared/binding';
import type { UserAssertionClaims } from '@runbook/runner-shared/contracts/core';
import { FakeCore } from '@runbook/runner-shared/test-utils/fake-core';
import type { MaterialPush } from '@runbook/runner-shared/contracts/internal';
import { sealSecret } from '@runbook/runner-shared/seal-client';
import { hasTestDatabase } from '@runbook/runner-shared/test-utils/pg';
import { credentialBinding } from '../src/types/index.ts';
import { insertCredential, startHarness, GROUP, ORG, RUNNER, TOKENS, type Harness } from './helpers.ts';
import type { OAuthProvider } from '../src/oauth/provider.ts';

const mcp = 'https://mcp.example/tenant';
const auth = 'https://auth.example/tenant';
const scopes = ['data:read', 'data:write'];
const hash = (value: string) => createHash('sha256').update(value).digest('base64url');

describe.skipIf(!hasTestDatabase)('vault OAuth against the mock authorization server', () => {
  let vault: Harness;
  let core: FakeCore;
  const pushes: MaterialPush[] = [];
  let origin: string;
  let registrations = 0;
  let exchangeCount = 0;
  let refreshCount = 0;
  let transientRefresh = false;
  const gatewaySuccesses = new Map<string, number>();
  let refuseRegistration = false;
  let refuseFence = false;
  let advertisesIssuer = true;
  const revokedTokens = new Set<string>();
  let revocation: { authorization?: string; form: URLSearchParams } | undefined;
  const plans = new Map<string, 'invalid_grant' | 'invalid_client' | 'unauthorized_client' | 'html_403' | 'unavailable' | 'drop' | 'access_only'>();
  const identities = new Map<string, object>();
  const accessTokens = new Map<string, string>();
  const codes = new Map<string, { challenge: string; redirect: string }>();
  const provider = createServer(async (request, response) => {
    const output = (status: number, body: unknown) => {
      response.writeHead(status, { 'content-type': 'application/json' }).end(JSON.stringify(body));
    };
    const url = new URL(request.url ?? '/', 'http://test');
    const host = request.headers['x-fixture-host'];
    if (host === 'mcp.example') {
      if (url.pathname === '/tenant') {
        return response.writeHead(401, { 'www-authenticate': 'Bearer resource_metadata="https://mcp.example/.well-known/oauth-protected-resource/tenant"' }).end();
      }
      return output(200, { resource: 'https://mcp.example', authorization_servers: [auth] });
    }
    if (url.pathname === '/.well-known/oauth-authorization-server/tenant') {
      return output(200, { issuer: auth, authorization_endpoint: `${auth}/authorize`, token_endpoint: `${auth}/token`,
        registration_endpoint: `${auth}/register`, revocation_endpoint: 'https://revocation.example/revoke',
        authorization_response_iss_parameter_supported: advertisesIssuer, token_endpoint_auth_methods_supported: ['client_secret_basic'],
        code_challenge_methods_supported: ['S256'] });
    }
    const chunks: Buffer[] = [];
    for await (const part of request) chunks.push(part as Buffer);
    const body = Buffer.concat(chunks).toString();
    if (url.pathname === '/tenant/register') {
      registrations++;
      if (refuseRegistration) return output(503, { error: 'temporarily_unavailable' });
      const input = JSON.parse(body) as { grant_types: string[]; redirect_uris: string[]; token_endpoint_auth_method: string };
      if (!input.grant_types.includes('refresh_token') || !input.redirect_uris[0]?.startsWith('https://core.example/v1/runner/oauth/callback/') ||
          input.token_endpoint_auth_method !== 'client_secret_basic') return output(400, {});
      return output(201, { client_id: 'mock-client', client_secret: 'synthetic-client-secret-marker',
        registration_access_token: 'synthetic-registration-token-marker', client_secret_expires_at: 0 });
    }
    const form = new URLSearchParams(body);
    if (url.pathname === '/revoke') {
      revokedTokens.add(form.get('token')!);
      revocation = { authorization: request.headers.authorization, form };
      return output(200, {});
    }
    if (url.pathname !== '/tenant/token' || request.headers.authorization !==
        `Basic ${Buffer.from('mock-client:synthetic-client-secret-marker').toString('base64')}` ||
        form.get('resource') !== 'https://mcp.example') return output(400, { error: 'invalid_request' });
    if (form.get('grant_type') === 'authorization_code') {
      exchangeCount++;
      const code = form.get('code') ?? '';
      const expected = codes.get(code);
      if (!expected || expected.challenge !== hash(form.get('code_verifier') ?? '') ||
          expected.redirect !== form.get('redirect_uri')) return output(400, { error: 'invalid_grant' });
      codes.delete(code);
      const plan = plans.get(code);
      if (code === 'synthetic-exchange-error-marker') return output(400, { error: 'invalid_grant', error_description: code });
      return output(200, { token_type: 'Bearer', access_token: accessTokens.get(code) ?? `synthetic-access-${code}`, expires_in: 3600,
        ...(plan !== 'access_only' && { refresh_token: `synthetic-refresh-${code}` }), scope: 'data:read', ...(identities.get(code) ?? { connected_account: 'Fixture User' }) });
    }
    refreshCount++;
    if (transientRefresh) return output(503, { error: 'temporarily_unavailable' });
    const refresh = form.get('refresh_token') ?? '';
    const code = refresh.replace('synthetic-refresh-', '');
    if (plans.get(code) === 'unavailable') return output(503, { error: 'temporarily_unavailable' });
    if (plans.get(code) === 'html_403') return response.writeHead(403).end('<html>Denied</html>');
    if (plans.get(code) === 'invalid_client' || plans.get(code) === 'unauthorized_client') return output(400, { error: plans.get(code) });
    if (plans.get(code) === 'drop') return request.socket.destroy();
    if (plans.get(code) === 'invalid_grant') return output(400, { error: 'invalid_grant' });
    return output(200, { token_type: 'bearer', access_token: `synthetic-fresh-access-${refreshCount}`,
      refresh_token: `synthetic-rotating-refresh-${refreshCount}`, expires_in: 3600,
      ...(code === 'fixture-workspace-name-first' && { workspace_name: 'Refreshed Workspace' }) });
  });

  const transport: OAuthProvider = {
    async check(url) {
      if (!['mcp.example', 'auth.example', 'revocation.example'].includes(new URL(url).hostname)) throw new Error('test destination denied');
    },
    async request(url, options, readBody = true) {
      const target = new URL(url);
      await this.check(url);
      const response = await fetch(origin + target.pathname + target.search, {
        ...options, headers: { ...options?.headers, 'x-fixture-host': target.hostname }, redirect: 'error',
      });
      const body = readBody ? await response.json().catch(() => null) as unknown : null;
      if (!readBody) await response.body?.cancel();
      return { status: response.status, headers: response.headers, body };
    },
  };

  beforeAll(async () => {
    await new Promise<void>((resolve) => provider.listen(0, '127.0.0.1', resolve));
    origin = `http://127.0.0.1:${(provider.address() as AddressInfo).port}`;
    const clock = { now: Date.now() };
    core = await new FakeCore({ now: () => clock.now, releaseToken: TOKENS.release }).start();
    vault = await startHarness({
      clock, oauthProvider: transport, core: { baseUrl: core.url, releaseToken: TOKENS.release },
      gateway: { baseUrl: 'https://gateway.example', token: TOKENS.gateway,
        fetch: async (input, init) => {
          const path = new URL(String(input)).pathname;
          if (refuseFence && path.endsWith('/oauth-fence')) return new Response(null, { status: 503 });
          if (path.endsWith('/oauth-use')) return Response.json({
            last_success_ms: gatewaySuccesses.get(path.split('/').at(-2)!) ?? null,
          });
          if (path === '/internal/mcp/discover') return Response.json({ tools: [] });
          if (path === '/internal/material') pushes.push(JSON.parse(String(init?.body)) as MaterialPush);
          return path === '/internal/test' ? Response.json({ outcome: 'response', status: 200 }) : new Response(null, { status: 204 });
        } },
    });
  });

  afterAll(async () => {
    await vault?.close();
    await core?.close();
    provider.closeAllConnections();
    await new Promise<void>((resolve) => provider.close(() => resolve()));
  });

  async function call(id: string, operation: string, body: object = {}, claims?: Partial<UserAssertionClaims>) {
    const payload = JSON.stringify(body);
    const target = operation === 'callback' ? `/runner/v1/oauth/callback/${id}` : `/runner/v1/credentials/${id}/${operation}`;
    const response = await fetch(`${vault.url}/internal/oauth/${id}/${operation}`, {
      method: 'POST', headers: { authorization: `Bearer ${TOKENS.api}`, 'content-type': 'application/json',
        'runbook-assertion': vault.signAssertion(target, payload, { session_hash: hash('fixture-session'), fresh_mfa: true, ...claims }) },
      body: payload,
    });
    return { status: response.status, json: await response.json() as Record<string, unknown> };
  }

  async function callbackAndConfirm(id: string, callback: object, claims?: Partial<UserAssertionClaims>) {
    const result = await call(id, 'callback', callback, claims);
    if (!result.json.awaiting_confirmation) return result;
    const row = await vault.vault.secrets.credential(id);
    const pin = bindingHash('mcp-oauth', credentialBinding('mcp-oauth', row.config, row.groups as 'all' | string[])).toString('base64url');
    return call(id, 'oauth-confirm', { binding_hash: pin, confirm_account: row.oauth_status?.account_confirmation }, claims);
  }

  async function credential() {
    const configured = { url: mcp, registration_mode: 'dynamic', scopes, extra_parameters: { prompt: 'consent' } };
    const { id } = await insertCredential(vault, { type: 'mcp-oauth', config: configured });
    const discovery = await call(id, 'oauth-discover');
    expect(discovery.status).toBe(200);
    const candidate = discovery.json.config as Record<string, unknown>;
    const pin = (config: unknown) => bindingHash('mcp-oauth', credentialBinding('mcp-oauth', config, 'all')).toString('base64url');
    const confirmation = await call(id, 'oauth-confirm', { binding_hash: pin(candidate) });
    expect(confirmation.status).toBe(200);
    return { id, binding_hash: pin(confirmation.json.config) };
  }

  async function prepare(code: string, plan?: 'invalid_grant' | 'invalid_client' | 'unauthorized_client' | 'html_403' | 'unavailable' | 'drop' | 'access_only') {
    const destination = await credential();
    const { id, binding_hash } = destination;
    const started = await call(id, 'connect', { binding_hash });
    expect(started.status).toBe(200);
    const authorization = new URL(String(started.json.authorization_url));
    expect(authorization.searchParams.get('code_challenge_method')).toBe('S256');
    expect(authorization.searchParams.get('prompt')).toBe('consent');
    expect(authorization.searchParams.get('resource')).toBe('https://mcp.example');
    const challenge = authorization.searchParams.get('code_challenge')!;
    codes.set(code, { challenge, redirect: authorization.searchParams.get('redirect_uri')! });
    if (plan) plans.set(code, plan);
    const callback = { nonce: started.json.nonce, state: authorization.searchParams.get('state'), code, iss: auth };
    return { ...destination, callback };
  }

  it('authenticates the pending authorization URL with the enrolled identity', async () => {
    const { id, binding_hash } = await credential();
    const started = await call(id, 'connect', { binding_hash });
    expect(started.status).toBe(200);
    const { authorization_url, nonce, expires_at_ms, identity_signature, request_id } = started.json;
    expect(identity_signature).toEqual(expect.any(String));
    const key = createPublicKey({ format: 'jwk', key: {
      kty: 'OKP', crv: 'Ed25519', x: vault.vault.identity.publicKey,
    } });
    const authentic = (url: string) => verify(null, lengthPrefixed([
      'runbook-runner/oauth-start/v1', ORG, vault.vault.installId!, id, binding_hash,
      url, hash(String(nonce)), hash('fixture-session'), String(request_id), String(expires_at_ms), '',
    ]), key, Buffer.from(String(identity_signature), 'base64url'));
    expect(authentic(String(authorization_url))).toBe(true);
    expect(authentic(String(authorization_url) + '&unexpected=1')).toBe(false);
  });

  it('registers once, exchanges a single PKCE code, redacts logs and revokes without cross-host client authentication', async () => {
    const before = registrations;
    const { id, binding_hash, callback } = await prepare('synthetic-oauth-code-log-marker');
    expect(registrations).toBe(before + 1);
    const previous = exchangeCount;
    const results = await Promise.all([callbackAndConfirm(id, callback), callbackAndConfirm(id, callback)]);
    expect(results.map(({ status }) => status).sort()).toEqual([200, 400]);
    expect(exchangeCount).toBe(previous + 1);
    const access = await vault.vault.oauth!.withAccess(id, vault.clock.now + 30_000, async (access) => access);
    expect(access.material).toEqual({ fields: {}, mcp_oauth: {
      access_token: 'synthetic-access-synthetic-oauth-code-log-marker', token_version: 1, generation: 1,
    } });
    expect(access.expiresAt).toBe(vault.clock.now + 30_000);
    expect((await call(id, 'connect', { binding_hash })).status).toBe(200);
    expect((await call(id, 'oauth-confirm', { binding_hash })).status).toBe(200);
    expect(registrations).toBe(before + 1);
    expect((await call(id, 'disconnect')).json.revoked).toBe(true);
    expect(revocation?.form.get('token')).toBe('synthetic-refresh-synthetic-oauth-code-log-marker');
    expect(revocation?.form.get('client_id')).toBeNull();
    expect(revocation?.authorization).toBeUndefined();
    for (const marker of [
      String(callback.state), callback.code, 'synthetic-client-secret-marker', 'synthetic-registration-token-marker',
      'synthetic-refresh-', 'synthetic-access-',
    ]) expect(vault.logs.text()).not.toContain(marker);
  });

  it('requires the initiating human and session, a browser nonce, a matching binding and a fresh factor at start', async () => {
    const { id, callback, binding_hash } = await prepare('fixture-identity');
    expect((await call(id, 'connect', { binding_hash }, { principal: 'agent' })).status).toBe(403);
    expect((await call(id, 'connect', { binding_hash }, { fresh_mfa: false })).status).toBe(403);
    expect((await call(id, 'connect', { binding_hash: hash('different-binding') })).status).toBe(409);
    expect((await callbackAndConfirm(id, callback, { user_id: 'other-admin' })).status).toBe(400);
    expect((await callbackAndConfirm(id, callback, { session_hash: hash('other-browser') })).status).toBe(400);
    expect((await callbackAndConfirm(id, { ...callback, nonce: undefined })).status).toBe(400);
    expect((await callbackAndConfirm(id, { ...callback, nonce: hash('other-nonce') })).status).toBe(400);
    expect((await callbackAndConfirm(id, callback)).json.connected).toBe(true);
    expect((await callbackAndConfirm(id, callback)).status).toBe(400);
  });

  it.each([undefined, 'https://wrong-issuer.example'])('consumes a callback with a missing or unbound issuer', async (iss) => {
    const { id, callback } = await prepare(`fixture-issuer-${String(Boolean(iss))}`);
    const before = exchangeCount;
    expect((await callbackAndConfirm(id, { ...callback, iss })).json.connected).toBe(false);
    expect((await callbackAndConfirm(id, callback)).status).toBe(400);
    expect(exchangeCount).toBe(before);
  });

  it('allows a missing issuer only when metadata does not advertise it', async () => {
    advertisesIssuer = false;
    try {
      const { id, callback } = await prepare('fixture-optional-issuer');
      expect((await callbackAndConfirm(id, { ...callback, iss: undefined })).json.connected).toBe(true);
    } finally {
      advertisesIssuer = true;
    }
  });

  it('refuses public seals of provider-managed fields', async () => {
    const { id, binding_hash } = await credential();
    const key = await vault.vault.ingestKey();
    for (const field of ['access_token', 'refresh_token', 'client_secret']) {
      const blob = await sealSecret(key, {
        orgId: ORG, credentialId: id, field, expectedVersion: 0,
        bindingHash: Buffer.from(binding_hash, 'base64url'), value: 'synthetic-seal-fixture', now: vault.clock.now,
      });
      expect((await vault.call('POST', '/internal/seal', { blob })).json.error.code).toBe('credential.provider_managed');
    }
    await expect(vault.db.pool('api').query('SELECT * FROM credential_oauth')).rejects.toMatchObject({ code: '42501' });
  });

  it('fences old material on a binding change and revokes using the confirmed endpoint', async () => {
    const { id, callback, binding_hash } = await prepare('fixture-rebind');
    expect((await callbackAndConfirm(id, callback)).json.connected).toBe(true);
    const changed = await vault.call('POST', `/internal/credential-metadata/${id}`, { config: { scopes: ['data:read'] }, groups: ['rg_default'] });
    expect(changed.status).toBe(200);
    expect(changed.json.status).toBe('reconnect_required');
    expect(changed.json.binding_hash).not.toBe(binding_hash);
    expect((await call(id, 'connect', { binding_hash: changed.json.binding_hash })).status).toBe(200);
    expect((await call(id, 'disconnect')).json.revoked).toBe(true);
    expect(revocation?.form.get('token')).toBe('synthetic-refresh-fixture-rebind');
  });

  it('does not exchange denied consent or an authorization whose destination changed', async () => {
    const { id, callback } = await prepare('fixture-tampered');
    const before = exchangeCount;
    await vault.db.admin().query("UPDATE credentials SET config = jsonb_set(config, '{token_endpoint}', '\"https://revocation.example/token\"') WHERE id = $1", [id]);
    expect((await callbackAndConfirm(id, callback)).json.connected).toBe(false);
    const denied = await prepare('fixture-denied');
    expect((await call(denied.id, 'callback', { ...denied.callback, code: undefined, error: 'access_denied' })).json.connected).toBe(false);
    expect(exchangeCount).toBe(before);
  });

  it('coalesces a 401 storm against a token version and rotates once', async () => {
    const { id, callback } = await prepare('fixture-rotate');
    expect((await callbackAndConfirm(id, callback)).json.connected).toBe(true);
    const before = refreshCount;
    const oauth = vault.vault.oauth!;
    await Promise.all(Array.from({ length: 10 }, () => oauth.ensureAccess(id, 'renewal', 1)));
    expect(refreshCount).toBe(before + 1);
    await oauth.ensureAccess(id, 'renewal', 2);
    expect(refreshCount).toBe(before + 1);
    const access = await oauth.withAccess(id, Infinity, async (access) => access);
    expect(access.material.mcp_oauth?.access_token).toBe(`synthetic-fresh-access-${refreshCount}`);
    expect(access.version).toBe(2);
  });

  it('pushes current OAuth material on a live lease renewal', async () => {
    const { id, callback } = await prepare('fixture-renewal');
    expect((await callbackAndConfirm(id, callback)).json.test).toEqual({ ok: true });
    const step = 'sr_oauth_fixture';
    const claim = core.lease(step, { credentials: [id] });
    await vault.db.admin().query(
      'INSERT INTO agents(id, project_id, group_id, name, status, current_version_id) VALUES ($1,$2,$3,$1,$4,$5)',
      [claim.agent_id, claim.project_id, GROUP, 'active', claim.agent_version],
    );
    await vault.db.admin().query(
      "INSERT INTO agent_versions(id, agent_id, version, config, ceiling, created_by) VALUES($1,$2,1,$3,'{}','us_admin')",
      [claim.agent_version, claim.agent_id, JSON.stringify({ credentials: [{ credential_id: id, delivery: 'mcp' }] })],
    );
    const cap = await vault.call('POST', '/internal/capability',
      { step_run_id: step, claim_id: claim.claim_id, generation: claim.generation, runner_id: RUNNER }, TOKENS.worker);
    expect(cap.status).toBe(200);
    const release = await vault.call('POST', '/internal/release',
      { capability: cap.json.capability, credential_id: id, delivery: 'mcp' }, TOKENS.worker);
    expect(release.status).toBe(200);
    const material = () => pushes.findLast((push) => push.release_id === release.json.release_id)?.material;
    expect(material()).toEqual({ fields: {}, mcp_oauth: { generation: 1, token_version: 1,
      access_token: 'synthetic-access-fixture-renewal' } });
    await vault.call('POST', '/internal/oauth/rejected', {
      capability: cap.json.capability, release_id: release.json.release_id, token_version: 1,
    }, TOKENS.worker);
    expect(material()?.mcp_oauth?.token_version).toBe(2);
    vault.clock.now += 450_000;
    claim.deadline = Math.floor(vault.clock.now / 1000) + 600;
    await vault.vault.releases!.renewer.runDue();
    const renewed = pushes.findLast((push) => push.release_id === release.json.release_id)!;
    expect(renewed.material.mcp_oauth?.token_version).toBe(2);
    expect(Date.parse(renewed.expires_at)).toBe(claim.deadline * 1000);
  });

  it('holds new grants until the initiating administrator confirms the provider account', async () => {
    const { id, callback, binding_hash } = await prepare('fixture-confirm-provider-account');
    const result = await call(id, 'callback', callback);
    expect(result.json).toMatchObject({ connected: false, awaiting_confirmation: true });
    const stored = await vault.vault.secrets.credential(id);
    expect(stored.status).not.toBe('active');
    expect(stored.oauth_status).toMatchObject({ pending_account: 'Fixture User' });
    const confirm = { binding_hash, confirm_account: stored.oauth_status?.account_confirmation };
    expect((await call(id, 'oauth-confirm', confirm, { user_id: 'different-admin' })).status).toBe(409);
    expect((await call(id, 'oauth-confirm', confirm)).json.connected).toBe(true);
    expect((await vault.vault.secrets.credential(id)).status).toBe('active');
  });

  it('accepts an explicit confirmation when the provider reports no account', async () => {
    const code = 'fixture-unreported-account';
    identities.set(code, {});
    const { id, callback } = await prepare(code);
    expect((await callbackAndConfirm(id, callback)).json.connected).toBe(true);
    expect((await vault.vault.secrets.credential(id)).oauth_status).toMatchObject({ connected_account: null });
  });

  it('requires the same full browser session to confirm the pending grant', async () => {
    const { id, callback, binding_hash } = await prepare('fixture-confirm-session');
    expect((await call(id, 'callback', callback)).json.awaiting_confirmation).toBe(true);
    const { oauth_status } = await vault.vault.secrets.credential(id);
    const confirm = { binding_hash, confirm_account: oauth_status?.account_confirmation };
    expect((await call(id, 'oauth-confirm', confirm, { session_hash: hash('other-session') })).status).toBe(409);
    expect((await call(id, 'oauth-confirm', confirm, { session_hash: undefined })).status).toBe(403);
    expect((await call(id, 'oauth-confirm', confirm)).json.connected).toBe(true);
  });

  it('accepts a Notion-shaped token and signs the provider account details', async () => {
    const code = 'fixture-signed-account';
    accessTokens.set(code, 'ntn_fixture:workspace-token');
    identities.set(code, { workspace_name: 'Exam\u202eple Work\u0000space', workspace_id: 'workspace-42',
      email_domain: 'example.com', bot_id: 'fixture-17' });
    const { id, callback, binding_hash } = await prepare(code);
    expect((await call(id, 'callback', callback)).json.awaiting_confirmation).toBe(true);
    const info = (await vault.vault.secrets.credential(id)).oauth_status!;
    expect(info.pending_account).toBe('Example Workspace (workspace-42) · example.com · bot fixture-17');
    const key = createPublicKey({ key: { kty: 'OKP', crv: 'Ed25519', x: vault.vault.identity.publicKey }, format: 'jwk' });
    const statement = lengthPrefixed(['runbook-runner/oauth-account/v1', ORG, vault.vault.installId!, id, binding_hash,
      String(info.account_confirmation), String(info.pending_account), String(info.account_changed),
      String(info.account_confirmation_expires_at_ms)]);
    expect(verify(null, statement, key, Buffer.from(info.account_confirmation_signature ?? '', 'base64url'))).toBe(true);
  });

  it('requires confirmation for a changed name when the provider reports no stable ID', async () => {
    const firstCode = 'fixture-workspace-name-first';
    identities.set(firstCode, { workspace_name: 'First Workspace', email: 'user@example.net' });
    const first = await prepare(firstCode);
    expect((await callbackAndConfirm(first.id, first.callback)).json.connected).toBe(true);
    await vault.vault.oauth!.ensureAccess(first.id, 'renewal', 1);
    expect((await vault.vault.secrets.credential(first.id)).oauth_status?.connected_account)
      .toBe('First Workspace (user@example.net)');

    const second = await call(first.id, 'connect', { binding_hash: first.binding_hash });
    const url = new URL(String(second.json.authorization_url));
    const code = 'fixture-workspace-name-second';
    codes.set(code, { challenge: url.searchParams.get('code_challenge')!, redirect: url.searchParams.get('redirect_uri')! });
    identities.set(code, { workspace_name: 'Second Workspace', email: 'user@example.net' });
    expect((await call(first.id, 'callback', { code, nonce: second.json.nonce, iss: auth,
      state: url.searchParams.get('state') })).json.awaiting_confirmation).toBe(true);
    const info = (await vault.vault.secrets.credential(first.id)).oauth_status;
    expect(info).toMatchObject({ connected_account: 'First Workspace (user@example.net)',
      pending_account: 'Second Workspace (user@example.net)', account_changed: true });
  });

  it.each(['restart', 'disconnect', 'expiry'])('revokes an unconfirmed grant discarded by %s', async (disposition) => {
    const code = `fixture-unconfirmed-${disposition}`;
    const { id, callback, binding_hash } = await prepare(code);
    expect((await call(id, 'callback', callback)).json.awaiting_confirmation).toBe(true);
    revocation = undefined;
    if (disposition === 'restart') expect((await call(id, 'connect', { binding_hash })).status).toBe(200);
    else if (disposition === 'disconnect') expect((await call(id, 'disconnect')).status).toBe(200);
    else {
      vault.clock.now += 600_001;
      await vault.vault.oauth!.maintain();
    }
    expect(revokedTokens.has(`synthetic-refresh-${code}`)).toBe(true);
  });

  it('releases the previous grant only after the replacement account is confirmed', async () => {
    const first = await prepare('fixture-old-grant');
    expect((await callbackAndConfirm(first.id, first.callback)).json.connected).toBe(true);
    const second = await call(first.id, 'connect', { binding_hash: first.binding_hash });
    const url = new URL(String(second.json.authorization_url));
    const code = 'fixture-new-grant';
    codes.set(code, { challenge: url.searchParams.get('code_challenge')!, redirect: url.searchParams.get('redirect_uri')! });
    revocation = undefined;
    expect((await call(first.id, 'callback', { code, nonce: second.json.nonce, iss: auth,
      state: url.searchParams.get('state') })).json.awaiting_confirmation).toBe(true);
    expect(revocation).toBeUndefined();
    const { oauth_status } = await vault.vault.secrets.credential(first.id);
    expect((await call(first.id, 'oauth-confirm', { binding_hash: first.binding_hash,
      confirm_account: oauth_status?.account_confirmation })).json.connected).toBe(true);
    expect(revocation).toMatchObject({ form: expect.any(URLSearchParams) });
    expect((revocation as { form: URLSearchParams } | undefined)?.form.get('token')).toBe('synthetic-refresh-fixture-old-grant');
  });

  it('returns the signed authorization only to the matching CLI handoff and initiating user', async () => {
    const { id, binding_hash } = await credential();
    const authorization_handoff = hash('cli-handoff');
    const input = { binding_hash, authorization_handoff };
    expect((await call(id, 'oauth-confirm', input)).json.authorization).toBeUndefined();
    const started = await call(id, 'connect', input);
    expect(started.status).toBe(200);
    expect((await call(id, 'oauth-confirm', { ...input, authorization_handoff: hash('other') })).json.authorization).toBeUndefined();
    expect((await call(id, 'oauth-confirm', input, { user_id: 'different-admin' })).json.authorization).toBeUndefined();
    const response = (await call(id, 'oauth-confirm', input)).json.authorization;
    expect(response).toMatchObject({ authorization_url: started.json.authorization_url,
      identity_signature: started.json.identity_signature });
    expect(response).not.toHaveProperty('nonce');
  });

  it('backs off consecutive forced refreshes even when the provider succeeds', async () => {
    const { id, callback } = await prepare('fixture-refresh-backoff');
    expect((await callbackAndConfirm(id, callback)).json.connected).toBe(true);
    const before = refreshCount;
    const oauth = vault.vault.oauth!;
    await oauth.ensureAccess(id, 'renewal', 1);
    vault.clock.now += 30_000;
    await oauth.ensureAccess(id, 'renewal', 2);
    vault.clock.now += 30_000;
    await oauth.ensureAccess(id, 'renewal', 3);
    expect(refreshCount).toBe(before + 2);
    vault.clock.now += 30_000;
    await oauth.ensureAccess(id, 'renewal', 3);
    expect(refreshCount).toBe(before + 3);
  });

  it('records an exchange failure when the new connection cannot fence the previous generation', async () => {
    const { id, callback, binding_hash } = await prepare('fixture-connect-fence-fails');
    await call(id, 'callback', callback);
    const { oauth_status } = await vault.vault.secrets.credential(id);
    refuseFence = true;
    try {
      expect((await call(id, 'oauth-confirm', { binding_hash, confirm_account: oauth_status?.account_confirmation })).status).toBe(502);
    } finally { refuseFence = false; }
    expect((await vault.vault.secrets.credential(id)).oauth_status?.connect_result).toBe('exchange_failed');
  });

  it('reports a pending purge until the gateway acknowledges the fence', async () => {
    const { id, callback } = await prepare('fixture-unreachable-fence');
    expect((await callbackAndConfirm(id, callback)).json.connected).toBe(true);
    refuseFence = true;
    try {
      const pending = await call(id, 'disconnect');
      expect(pending.status).toBe(202);
      expect(pending.json).toMatchObject({ gateway_purged: false, pending_purge: true,
        access_token_remaining_ms: 3_600_000 });
      expect((await call(id, 'disconnect')).json.pending_purge).toBe(true);
    } finally { refuseFence = false; }
    await vault.vault.oauth!.maintain();
    expect((await call(id, 'disconnect')).json).toMatchObject({ gateway_purged: true, pending_purge: false });
  });

  it('retries transient refresh failures with backoff instead of disconnecting', async () => {
    const { id, callback } = await prepare('fixture-transient-refresh');
    expect((await callbackAndConfirm(id, callback)).json.connected).toBe(true);
    const oauth = vault.vault.oauth!;
    const before = refreshCount;
    transientRefresh = true;
    try {
      await expect(oauth.ensureAccess(id, 'renewal', 1)).rejects.toMatchObject({ code: 'oauth.refresh_retry' });
      expect((await vault.vault.secrets.credential(id)).status).toBe('active');
      await oauth.ensureAccess(id, 'renewal', 1);
      expect(refreshCount).toBe(before + 1);
    } finally { transientRefresh = false; }
    vault.clock.now += 30_000;
    await oauth.ensureAccess(id, 'renewal', 1);
    expect(refreshCount).toBe(before + 2);
  });

  it.each(['invalid_client', 'unauthorized_client'] as const)('requires reconnection on %s', async (failure) => {
    const { id, callback } = await prepare(`fixture-${failure}`, failure);
    expect((await callbackAndConfirm(id, callback)).json.connected).toBe(true);
    await expect(vault.vault.oauth!.ensureAccess(id, 'renewal', 1)).rejects.toMatchObject({ code: 'oauth.reconnect_required' });
    expect(await vault.vault.secrets.credential(id)).toMatchObject({
      status: 'reconnect_required', oauth_status: { reconnect_reason: failure },
    });
  });

  it('alerts once after an hour of refresh failures and requires reconnection after a day', async () => {
    const { id, callback } = await prepare('fixture-persistent-refresh', 'unavailable');
    expect((await callbackAndConfirm(id, callback)).json.connected).toBe(true);
    const oauth = vault.vault.oauth!;
    const alerts = async () => (await vault.db.admin().query(
      `SELECT kind FROM budget_event_outbox WHERE data->>'credential_id' = $1 ORDER BY kind`, [id],
    )).rows;
    await expect(oauth.ensureAccess(id, 'renewal', 1)).rejects.toMatchObject({ code: 'oauth.refresh_retry' });
    expect((await vault.vault.secrets.credential(id)).oauth_status).toMatchObject({
      refresh_failures: 1, last_refresh_error: 'provider_unavailable',
    });
    vault.clock.now += 3_600_000;
    await oauth.maintain();
    await oauth.maintain();
    expect(await alerts()).toEqual([{ kind: 'credential.oauth_refresh_failing' }]);
    vault.clock.now += 23 * 3_600_000;
    await oauth.maintain();
    expect(await vault.vault.secrets.credential(id)).toMatchObject({
      status: 'reconnect_required', oauth_status: { reconnect_reason: 'refresh_failure_timeout' },
    });
  });

  it('retries an unstructured 4xx without disconnecting', async () => {
    const { id, callback } = await prepare('fixture-waf-refresh', 'html_403');
    expect((await callbackAndConfirm(id, callback)).json.connected).toBe(true);
    await expect(vault.vault.oauth!.ensureAccess(id, 'renewal', 1)).rejects.toMatchObject({ code: 'oauth.refresh_retry' });
    expect((await vault.vault.secrets.credential(id)).status).toBe('active');
    await call(id, 'disconnect');
  });

  it('keeps valid access available when a scheduled refresh fails', async () => {
    const { id, callback } = await prepare('fixture-due-refresh');
    expect((await callbackAndConfirm(id, callback)).json.connected).toBe(true);
    vault.clock.now += 1_800_000;
    const before = refreshCount;
    transientRefresh = true;
    try {
      await vault.vault.oauth!.ensureAccess(id, 'release');
      expect(refreshCount).toBe(before + 1);
    } finally { transientRefresh = false; }
    await call(id, 'disconnect');
  });

  it('starts a new access-only rejection window after a quiet interval', async () => {
    const { id, callback } = await prepare('fixture-old-rejection', 'access_only');
    expect((await callbackAndConfirm(id, callback)).json.connected).toBe(true);
    const oauth = vault.vault.oauth!;
    await oauth.ensureAccess(id, 'renewal', 1);
    vault.clock.now += 900_000;
    await oauth.ensureAccess(id, 'renewal', 1);
    expect((await vault.vault.secrets.credential(id)).status).toBe('active');
    vault.clock.now += 30_000;
    await expect(oauth.ensureAccess(id, 'renewal', 1)).rejects.toMatchObject({ code: 'oauth.reconnect_required' });
  });

  it.each(['connection test', 'tool discovery'])('clears an access-only rejection after successful %s', async (operation) => {
    const { id, callback } = await prepare(`fixture-rejection-recovered-${operation.replace(' ', '-')}`, 'access_only');
    expect((await callbackAndConfirm(id, callback)).json.connected).toBe(true);
    const oauth = vault.vault.oauth!;
    await oauth.ensureAccess(id, 'renewal', 1);
    if (operation === 'connection test') {
      expect((await vault.vault.releases!.test({ credential_id: id }, { kind: 'user', id: 'admin' })).ok).toBe(true);
    } else {
      expect(await vault.vault.releases!.discover({ credential_id: id, url: mcp, transport: 'streamable-http' }, true))
        .toEqual({ tools: [] });
    }
    vault.clock.now += 30_000;
    await oauth.ensureAccess(id, 'renewal', 1);
    expect((await vault.vault.secrets.credential(id)).status).toBe('active');
  });

  it('clears an access-only rejection after a verified live gateway use', async () => {
    const { id, callback } = await prepare('fixture-live-rejection-recovered', 'access_only');
    expect((await callbackAndConfirm(id, callback)).json.connected).toBe(true);
    const oauth = vault.vault.oauth!;
    await oauth.ensureAccess(id, 'renewal', 1);
    vault.clock.now += 15_000;
    gatewaySuccesses.set(id, vault.clock.now);
    vault.clock.now += 15_000;
    await oauth.maintain();
    const state = async () => (await vault.db.admin().query(
      'SELECT access_rejected_since FROM credential_oauth WHERE credential_id = $1', [id],
    )).rows[0];
    expect(await state()).toEqual({ access_rejected_since: null });
    await oauth.ensureAccess(id, 'renewal', 1);
    expect((await vault.vault.secrets.credential(id)).status).toBe('active');
  });

  it('keeps an access-only grant active after a single rejection', async () => {
    const { id, callback } = await prepare('fixture-access-only-rejected-once', 'access_only');
    expect((await callbackAndConfirm(id, callback)).json.connected).toBe(true);
    await vault.vault.oauth!.ensureAccess(id, 'renewal', 1);
    expect((await vault.vault.secrets.credential(id)).status).toBe('active');
  });

  it('records safe audit events and log lines for callback and exchange failures', async () => {
    const code = 'synthetic-exchange-error-marker';
    const { id, callback } = await prepare(code);
    expect((await call(id, 'callback', { ...callback, nonce: undefined })).status).toBe(400);
    expect((await call(id, 'callback', callback)).status).toBe(409);
    const { rows } = await vault.db.admin().query(`SELECT action, detail FROM audit_log
      WHERE resource_id = $1 AND action LIKE 'credential.oauth_%_failed' ORDER BY seq`, [id]);
    expect(rows).toEqual([
      { action: 'credential.oauth_callback_failed', detail: { error_class: 'oauth.invalid_callback' } },
      { action: 'credential.oauth_exchange_failed', detail: { error_class: 'oauth.invalid_grant' } },
    ]);
    expect(vault.logs.text()).toContain('"error_class":"oauth.invalid_grant","provider_status":400');
    for (const marker of [code, callback.nonce, callback.state]) {
      expect(vault.logs.text()).not.toContain(marker);
      expect(JSON.stringify(rows)).not.toContain(marker);
    }
  });

  it('persists OAuth discovery, replaces the candidate and clears it after confirmation', async () => {
    const { id } = await insertCredential(vault, { type: 'mcp-oauth', config: { url: mcp, registration_mode: 'dynamic', scopes } });
    const read = async () => (await vault.db.pool('api').query('SELECT oauth_status FROM credentials WHERE id = $1', [id]))
      .rows[0]?.oauth_status?.discovery_candidate;
    const discovery = await call(id, 'oauth-discover');
    expect(await read()).toEqual(discovery.json);
    // Mimic a draft discovered by the previous vault version.
    await vault.db.pool('vault').query('UPDATE credentials SET oauth_status = NULL WHERE id = $1', [id]);
    await vault.vault.oauth!.restoreDiscoveryCandidates(ORG);
    expect(await read()).toEqual(discovery.json);
    let replacement;
    advertisesIssuer = false;
    try {
      replacement = await call(id, 'oauth-discover');
    } finally {
      advertisesIssuer = true;
    }
    expect(replacement.json.config).toMatchObject({ iss_supported: false });
    expect(await read()).toEqual(replacement.json);
    const pin = (config: unknown) => bindingHash('mcp-oauth', credentialBinding('mcp-oauth', config, 'all')).toString('base64url');
    expect((await call(id, 'oauth-confirm', { binding_hash: pin(discovery.json.config) })).status).toBe(409);
    expect((await call(id, 'oauth-confirm', { binding_hash: pin(replacement.json.config) })).status).toBe(200);
    expect(await read()).toBeUndefined();
  });

  it('allows retry after an explicit dynamic registration refusal', async () => {
    const { id } = await insertCredential(vault, { type: 'mcp-oauth', config: { url: mcp, registration_mode: 'dynamic', scopes } });
    const discovery = await call(id, 'oauth-discover');
    const pin = bindingHash('mcp-oauth', credentialBinding('mcp-oauth', discovery.json.config, 'all')).toString('base64url');
    refuseRegistration = true;
    try {
      expect((await call(id, 'oauth-confirm', { binding_hash: pin })).status).toBe(502);
    } finally { refuseRegistration = false; }
    expect((await call(id, 'oauth-confirm', { binding_hash: pin })).status).toBe(200);
  });

  it.each(['drop', 'invalid_grant'] as const)('never retries a refresh token after %s', async (plan) => {
    const { id, callback } = await prepare(`fixture-${plan}`, plan);
    expect((await callbackAndConfirm(id, callback)).json.connected).toBe(true);
    const before = refreshCount;
    const oauth = vault.vault.oauth!;
    await expect(oauth.ensureAccess(id, 'renewal', 1)).rejects.toMatchObject({ code: 'oauth.reconnect_required' });
    await expect(oauth.ensureAccess(id, 'renewal', 1)).rejects.toMatchObject({ code: 'oauth.reconnect_required' });
    expect(refreshCount).toBe(before + 1);
    expect((await vault.vault.secrets.credential(id)).status).toBe('reconnect_required');
    const { rows } = await vault.db.admin().query("SELECT kind FROM budget_event_outbox WHERE data->>'credential_id' = $1", [id]);
    expect(rows).toEqual([{ kind: 'credential.reconnect_required' }]);
  });

  it('warns for a hard grant expiry and never refreshes once the grant ends', async () => {
    const { id, callback } = await prepare('fixture-hard-limit');
    expect((await callbackAndConfirm(id, callback)).json.connected).toBe(true);
    await vault.db.admin().query('UPDATE credential_oauth SET grant_expires_at = $2 WHERE credential_id = $1',
      [id, new Date(vault.clock.now + 7 * 86_400_000)]);
    const oauth = vault.vault.oauth!;
    const before = refreshCount;
    await oauth.maintain();
    const { rows } = await vault.db.admin().query("SELECT kind FROM budget_event_outbox WHERE data->>'credential_id' = $1", [id]);
    expect(rows).toEqual([{ kind: 'credential.oauth_expiring' }]);
    vault.clock.now += 7 * 86_400_000;
    await expect(oauth.ensureAccess(id, 'release')).rejects.toMatchObject({ code: 'oauth.reconnect_required' });
    expect(refreshCount).toBe(before);
  });

  it('caps access-only material and blocks at hard expiry', async () => {
    const { id, callback } = await prepare('fixture-access-only', 'access_only');
    expect((await callbackAndConfirm(id, callback)).json.connected).toBe(true);
    const oauth = vault.vault.oauth!;
    const expiry = vault.clock.now + 3_600_000;
    expect(await oauth.withAccess(id, Infinity, async ({ expiresAt }) => expiresAt)).toBe(expiry);
    const before = refreshCount;
    vault.clock.now = expiry;
    await expect(oauth.ensureAccess(id, 'keep-alive')).rejects.toMatchObject({ code: 'oauth.reconnect_required' });
    expect(refreshCount).toBe(before);
  });
});
