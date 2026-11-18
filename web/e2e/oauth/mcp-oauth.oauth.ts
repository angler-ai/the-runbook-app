// MCP2 review fix 1: a person connects an MCP client in a real browser. The
// sign-in and consent pages are plain HTML forms, so this is the test that
// sees what browsers send (Origin and Referer on a form post), which a
// fetch-based test sets by hand. Run with `npm run e2e:oauth` (both stacks).
import { expect, test } from '@playwright/test';
import { createHash, randomBytes } from 'node:crypto';
import { createServer, type Server } from 'node:http';
import { OWNER, startNative, startWorkers, type OAuthStack } from './stacks.ts';

let stack: OAuthStack;
// The MCP client's loopback redirect (RFC 8252): a real listener, as a
// desktop client runs one.
let callback: Server;
let REDIRECT = '';

test.beforeAll(async () => {
  const info = test.info();
  test.setTimeout(180_000);
  callback = createServer((_, res) => {
    res.writeHead(200, { 'content-type': 'text/plain' });
    res.end('callback');
  });
  await new Promise<void>((resolve) => callback.listen(0, '127.0.0.1', resolve));
  const address = callback.address();
  REDIRECT = `http://127.0.0.1:${typeof address === 'object' && address ? address.port : 0}/callback`;
  stack = info.project.name === 'workers' ? await startWorkers() : await startNative();
});
test.afterAll(async () => {
  await stack?.stop();
  await new Promise((resolve) => callback?.close(resolve));
});

async function json(response: Response): Promise<Record<string, unknown>> {
  const text = await response.text();
  expect(response.ok, text).toBe(true);
  return JSON.parse(text) as Record<string, unknown>;
}

test('a person signs in, consents and the MCP client calls /mcp with the token', async ({ page }) => {
  const origin = stack.origin;
  const client = await json(
    await fetch(`${origin}/api/auth/oauth2/register`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ client_name: 'Browser test client', redirect_uris: [REDIRECT], token_endpoint_auth_method: 'none' }),
    }),
  );
  const verifier = randomBytes(32).toString('base64url');
  const challenge = createHash('sha256').update(verifier).digest('base64url');
  const params = new URLSearchParams({
    response_type: 'code',
    client_id: String(client.client_id),
    redirect_uri: REDIRECT,
    state: 'browser-1',
    code_challenge: challenge,
    code_challenge_method: 'S256',
    scope: 'runbooks:read runbooks:publish executions:read offline_access',
    resource: `${origin}/mcp`,
  });
  const refused: string[] = [];
  page.on('response', (r) => {
    if (r.status() === 403) refused.push(`${r.request().method()} ${r.url()}`);
  });

  await page.goto(`${origin}/api/auth/oauth2/authorize?${params}`);
  await expect(page.getByRole('heading', { name: 'Sign in to The Runbook' })).toBeVisible();
  await page.getByLabel('Email').fill(OWNER);
  await page.getByRole('button', { name: 'Send code' }).click();
  await expect(page.getByRole('heading', { name: 'Check your email' })).toBeVisible();
  await page.getByLabel('Code').fill(await stack.code(OWNER));
  await page.getByRole('button', { name: 'Sign in' }).click();

  await expect(page.getByRole('heading', { name: /to use The Runbook\?$/ })).toBeVisible();
  await expect(page.getByRole('checkbox', { name: /Publish runbook versions/ })).not.toBeChecked();
  await expect(page.getByRole('checkbox', { name: /Find and read runbooks/ })).toBeChecked();
  await page.getByRole('button', { name: 'Allow' }).click();
  await page.waitForURL((url) => url.href.startsWith(REDIRECT));
  expect(refused, 'no form post was refused').toEqual([]);

  const back = new URL(page.url());
  expect(back.searchParams.get('state')).toBe('browser-1');
  const tokens = await json(
    await fetch(`${origin}/api/auth/oauth2/token`, {
      method: 'POST',
      headers: { 'content-type': 'application/x-www-form-urlencoded' },
      body: new URLSearchParams({
        grant_type: 'authorization_code',
        client_id: String(client.client_id),
        code: back.searchParams.get('code') ?? '',
        code_verifier: verifier,
        redirect_uri: REDIRECT,
        resource: `${origin}/mcp`,
      }).toString(),
    }),
  );
  expect(String(tokens.access_token)).toMatch(/^trb_oat_/);
  expect(String(tokens.scope).split(' ')).not.toContain('runbooks:publish');
  const mcp = (message: unknown) =>
    fetch(`${origin}/mcp`, {
      method: 'POST',
      headers: { authorization: `Bearer ${String(tokens.access_token)}`, 'content-type': 'application/json', accept: 'application/json' },
      body: JSON.stringify(message),
    });
  const init = await json(
    await mcp({ jsonrpc: '2.0', id: 0, method: 'initialize', params: { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'e2e', version: '1' } } }),
  );
  expect(init.result).toBeTruthy();
  // resources/list walks /v1/orgs and the catalogs with the token.
  const listed = await json(await mcp({ jsonrpc: '2.0', id: 1, method: 'resources/list', params: {} }));
  expect(listed.error, JSON.stringify(listed)).toBeUndefined();
});
