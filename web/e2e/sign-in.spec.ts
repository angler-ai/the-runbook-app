// Sign-in (A3, F90, U3): only a human, personal token whose scopes are all
// on the browser allow-list gets in, sign-out revokes the token it signed
// in with, and the next person in the tab sees nothing of the last (U1).
import type { Page, Route } from '@playwright/test';
import { join } from 'node:path';
import { apiGet, mintToken } from './support/cli.ts';
import {
  NAMES,
  expect,
  signIn,
  signInWithFile,
  signOutFromShell,
  test,
} from './support/fixtures.ts';
import { readSecret, type E2EState, type Person } from './support/state.ts';

const SUGGESTION = 'runbook token create --name browser --scope runbooks:read';
const NARROW = ['runbooks:read', 'executions:read', 'executions:start', 'executions:act'];

let minted = 0;
function tokenFile(stack: E2EState) {
  minted += 1;
  return join(stack.tokensDir, `sign-in-${process.pid}-${minted}.token`);
}

async function mint(stack: E2EState, options: { scopes: string[]; agent?: boolean; who?: Person }) {
  const file = tokenFile(stack);
  await mintToken(stack, options.who ?? 'alice', file, {
    name: `sign-in-e2e-${minted}`,
    seed: 'seed-web',
    scopes: options.scopes,
    ...(options.agent ? { agent: true } : {}),
  });
  return file;
}

/**
 * A token minted by the owner. The bootstrap owner PAT (pilot/owner.pat)
 * holds every scope, and a token never exceeds its caller, so it is the
 * only one that can mint webhooks:manage (and, besides the controller's
 * seed, executions:admin). The sign-in tests mint from it so the shared
 * `<controller>.control` token is never revoked by a sign-out here.
 */
async function mintAsOwner(stack: E2EState, scopes: string[]) {
  const file = tokenFile(stack);
  await mintToken(stack, 'owner' as Person, file, {
    name: `sign-in-e2e-owner-${minted}`,
    seed: 'pat' as 'seed',
    scopes,
  });
  return file;
}

async function runbookTabKeys(page: Page): Promise<string[]> {
  return page.evaluate(() => Object.keys(sessionStorage).filter((k) => k.startsWith('runbook.')));
}

test.describe('sign-in', () => {
  test('a narrow human PAT signs in and lands on My tasks', async ({ page, stack }) => {
    await signIn(page, stack, 'alice');
    await expect(page.getByText('Alice', { exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: /^Account:/ })).toBeVisible();
  });

  test('a token holding tokens:manage is refused with guidance', async ({ page, stack }) => {
    // The pilot seed: human, personal, executions:* plus tokens:manage.
    await signInWithFile(page, join(stack.pilotDir, 'alice.seed'));
    const alert = page.getByRole('alert');
    await expect(alert).toContainText(
      'That token carries tokens:manage, which is too powerful to keep in a browser.',
    );
    await expect(alert).toContainText(SUGGESTION);
    await expect(page).toHaveURL(/\/sign-in$/);
    expect(
      await page.evaluate(() => sessionStorage.getItem('runbook.session.token.v1')),
    ).toBeNull();
  });

  test('an agent-kind PAT is refused', async ({ page, stack }) => {
    const file = await mint(stack, {
      scopes: ['executions:read', 'executions:act'],
      agent: true,
    });
    await signInWithFile(page, file);
    const alert = page.getByRole('alert');
    await expect(alert).toContainText(
      'That is an agent-kind token, and the web app signs in humans.',
    );
    await expect(alert).toContainText(SUGGESTION);
    await expect(page).toHaveURL(/\/sign-in$/);
  });

  test('sign-out revokes the token: /v1/me then answers 401', async ({ page, stack }) => {
    // A token of its own, so revoking it leaves the shared browser tokens alone.
    const file = await mint(stack, { scopes: NARROW });
    expect((await apiGet(stack, file, '/v1/me')).status).toBe(200);
    await signInWithFile(page, file);
    await expect(page).toHaveURL(/\/tasks$/);

    const revoke = page.waitForResponse(
      (r) => r.url().endsWith('/v1/me/token') && r.request().method() === 'DELETE',
    );
    await signOutFromShell(page);
    expect((await revoke).ok()).toBe(true);
    await expect(page).toHaveURL(/\/sign-in\?reason=signed-out$/);
    await expect(page.getByRole('status')).toContainText(
      'You have signed out. That token was revoked',
    );
    expect(
      await page.evaluate(() => sessionStorage.getItem('runbook.session.token.v1')),
    ).toBeNull();

    expect((await apiGet(stack, file, '/v1/me')).status).toBe(401);
    // And the app no longer lets that token back in.
    await signInWithFile(page, file);
    await expect(page.getByRole('alert')).toContainText('The server did not accept that token.');
  });

  test('a token holding webhooks:manage is refused (U3)', async ({ page, stack }) => {
    const file = await mintAsOwner(stack, [...NARROW, 'webhooks:manage']);
    await signInWithFile(page, file);
    const alert = page.getByRole('alert');
    await expect(alert).toContainText(
      'That token carries webhooks:manage, which is too powerful to keep in a browser.',
    );
    await expect(alert).toContainText(SUGGESTION);
    await expect(page).toHaveURL(/\/sign-in$/);
    expect(await runbookTabKeys(page)).toEqual([]);
  });

  test('a token with executions:admin is accepted (U3, D27)', async ({ page, stack }) => {
    const file = await mintAsOwner(stack, [...NARROW, 'executions:admin']);
    await signInWithFile(page, file);
    await expect(page).toHaveURL(/\/tasks$/);
    await expect(page.getByRole('heading', { name: 'My tasks' })).toBeVisible();
    await expect(page.getByRole('button', { name: /^Account:/ })).toBeVisible();
  });

  test('after sign-out, the next person sees nothing of the first (U1)', async ({
    page,
    stack,
  }) => {
    // Tokens of their own: sign-out revokes, and the shared browser tokens must survive.
    const alice = await mint(stack, { scopes: NARROW, who: 'alice' });
    const bob = await mint(stack, { scopes: NARROW, who: 'bob' });

    await signInWithFile(page, alice);
    await expect(page).toHaveURL(/\/tasks$/);
    await expect(page.getByText(NAMES.alice, { exact: true })).toBeVisible();
    // Something of Alice's own in the tab: a palette recent.
    await page.getByRole('button', { name: /open command palette/i }).click();
    const palette = page.getByRole('dialog');
    await palette.getByRole('combobox').fill('post-deploy');
    await palette
      .getByRole('option', { name: /post-deploy/i })
      .first()
      .click();
    await expect.poll(() => runbookTabKeys(page)).toContain('runbook.palette.recents.v1');

    await signOutFromShell(page);
    await expect(page).toHaveURL(/\/sign-in\?reason=signed-out$/);
    expect(await runbookTabKeys(page)).toEqual([]);

    // Hold Bob's own reads after the sign-in probe, and look at the page meanwhile.
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    let meCalls = 0;
    const hold = async (route: Route) => {
      await gate;
      await route.continue();
    };
    await page.route('**/v1/me', async (route) => {
      meCalls += 1;
      if (meCalls === 1) await route.continue();
      else await hold(route);
    });
    await page.route('**/v1/me/tasks', hold);

    await page.getByLabel('Personal access token').fill(readSecret(bob));
    await page.getByRole('button', { name: 'Sign in' }).click();
    await expect(page).toHaveURL(/\/tasks$/);
    await expect(page.getByText('Loading your session…')).toBeAttached();
    await expect(page.getByText(NAMES.alice, { exact: true })).toHaveCount(0);

    release();
    await expect(page.getByText(NAMES.bob, { exact: true })).toBeVisible();
    await expect(page.getByText(NAMES.alice, { exact: true })).toHaveCount(0);
    await page.getByRole('button', { name: /open command palette/i }).click();
    await expect(page.getByRole('dialog').getByText('Actions', { exact: true })).toBeVisible();
    await expect(page.getByRole('dialog').getByText('Recent', { exact: true })).toHaveCount(0);
  });
});
