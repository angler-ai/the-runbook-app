// Router-level session lifecycle: what a 401, a refused restored token and
// a sign-out leave behind (U1, U14).
import { createMemoryHistory } from '@tanstack/react-router';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it } from 'vitest';
import { App } from '../../App';
import { tokenStore } from '../../api/token-store';
import { createAppRouter } from '../../routes/router';
import { server } from '../../test/msw/server';
import { resetTabSession, tabClaims, tabSessionId } from '../execution/session';
import { rememberRecent } from '../palette/PaletteHost';

interface Person {
  id: string;
  name: string;
  scopes?: string[];
}

const ALICE: Person = { id: 'usr_alice', name: 'Alice' };
const BOB: Person = { id: 'usr_bob', name: 'Bob' };
const ALICE_TOKEN = 'trb_pat_alice';
const BOB_TOKEN = 'trb_pat_bob';
const ALICE_TASK = 'Alice-only task: roll back checkout';

function meBody(person: Person) {
  return {
    user: { id: person.id, name: person.name },
    org: { id: 'org_1', title: 'Acme', role: 'operator' },
    teams: [],
    credential: {
      kind: 'personal',
      actor_kind: 'human',
      scopes: person.scopes ?? ['runbooks:read', 'executions:read'],
      projects: null,
      expires_at_ms: Date.now() + 3_600_000,
    },
  };
}

const unauthorized = () =>
  HttpResponse.json(
    {
      code: 'auth.invalid_credential',
      status: 401,
      detail: 'revoked',
      type: 't',
      title: 't',
      instance: 'urn:request:req_9',
      errors: [],
    },
    { status: 401, headers: { 'Content-Type': 'application/problem+json' } },
  );

const empty = () => HttpResponse.json({ items: [], next_cursor: null });

function directory() {
  return [
    http.get('*/v1/orgs', () =>
      HttpResponse.json({ items: [{ id: 'org_1', title: 'Acme' }], next_cursor: null }),
    ),
    http.get('*/v1/orgs/:org/projects', () =>
      HttpResponse.json({
        items: [{ id: 'prj_1', org_id: 'org_1', slug: 'prod', title: 'Production' }],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/orgs/:org/users', empty),
    http.get('*/v1/orgs/:org/teams', empty),
    http.get('*/v1/projects/:p/runbooks', empty),
    http.get('*/v1/projects/:p/executions', empty),
    http.get('*/v1/me/tasks', empty),
  ];
}

function renderAt(path: string) {
  const router = createAppRouter(createMemoryHistory({ initialEntries: [path] }));
  return { router, ...render(<App router={router} />) };
}

afterEach(() => {
  tokenStore.clear();
  sessionStorage.clear();
  resetTabSession();
});

function deferred() {
  let resolve: () => void = () => {};
  const promise = new Promise<void>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function runbookKeys(): string[] {
  const keys: string[] = [];
  for (let i = 0; i < sessionStorage.length; i += 1) {
    const key = sessionStorage.key(i);
    if (key?.startsWith('runbook.')) keys.push(key);
  }
  return keys;
}

/**
 * Two people on one tab. Bob's first /v1/me is the sign-in probe and is
 * answered at once; every later Bob read waits for `bobGate`, so the test
 * can look at the page while Bob's own data is still in flight. Alice's
 * inbox waits for `aliceTasksGate` and then answers `aliceTasks()`.
 */
function twoPeople(options: { aliceTasks?: () => Response } = {}) {
  const bobGate = deferred();
  const aliceTasksGate = deferred();
  let bobMeCalls = 0;
  const bearer = (request: Request) => request.headers.get('authorization')?.slice(7);
  server.use(
    http.get('*/v1/me', async ({ request }) => {
      if (bearer(request) === ALICE_TOKEN) return HttpResponse.json(meBody(ALICE));
      if (bearer(request) !== BOB_TOKEN) return unauthorized();
      bobMeCalls += 1;
      if (bobMeCalls > 1) await bobGate.promise;
      return HttpResponse.json(meBody(BOB));
    }),
    http.get('*/v1/me/tasks', async ({ request }) => {
      if (bearer(request) === ALICE_TOKEN) {
        await aliceTasksGate.promise;
        return (
          options.aliceTasks?.() ??
          HttpResponse.json({
            items: [
              {
                project_id: 'prj_1',
                execution_id: 'exe_alice',
                step_run_id: 'sr_alice',
                path: 'decide.signoff',
                title: ALICE_TASK,
                kind: 'approval',
              },
            ],
            next_cursor: null,
          })
        );
      }
      await bobGate.promise;
      return empty();
    }),
    http.delete('*/v1/me/token', () => new HttpResponse(null, { status: 204 })),
    ...directory(),
  );
  return { bobGate, aliceTasksGate };
}

/** What Alice's tab remembers besides the query cache; returns her tab session id. */
function aliceTabState(): string {
  const session = tabSessionId();
  tabClaims.set('sr_alice', { id: 'clm_alice', generation: 1 });
  rememberRecent({ kind: 'execution', id: 'exe_alice', label: 'Alice run', projectRef: 'prod' });
  expect(runbookKeys()).toEqual(
    expect.arrayContaining([
      'runbook.tab-session.v1',
      'runbook.tab-claims.v1',
      'runbook.palette.recents.v1',
    ]),
  );
  return session;
}

/**
 * Bob's claims go out under a session id of his own: the actor session the
 * tab sends with every claim (hooks.ts) is not the previous person's.
 */
async function expectFreshTabSession(aliceSession: string) {
  const bobSession = tabSessionId();
  expect(bobSession).toMatch(/^web-/);
  expect(bobSession).not.toBe(aliceSession);
  expect(sessionStorage.getItem('runbook.tab-session.v1')).toBe(bobSession);
}

async function signInAsBobAndLook(bobGate: { resolve(): void }) {
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText(/personal access token/i), BOB_TOKEN);
  await user.click(screen.getByRole('button', { name: /sign in/i }));
  // Bob's own /v1/me and inbox are still in flight: nothing of Alice's may show.
  expect(await screen.findByText(/loading your session/i)).toBeInTheDocument();
  expect(screen.queryByText('Alice')).toBeNull();
  expect(screen.queryByText(ALICE_TASK)).toBeNull();
  bobGate.resolve();
  expect(await screen.findByText('Bob')).toBeInTheDocument();
  // UI-2: a plain sign-in lands on Home, whose "Needs you" reads Bob's own
  // inbox; after a 401 it returns to the page Alice was on (the inbox).
  expect(await screen.findByText(/^(Inbox is clear|Nothing needs you\.)$/)).toBeInTheDocument();
  expect(screen.queryByText('Alice')).toBeNull();
  expect(screen.queryByText(ALICE_TASK)).toBeNull();
}

describe('a 401 keeps the destination (U14)', () => {
  it('a mid-session 401 returns to sign-in with next, and sign-in goes back there', async () => {
    let revoked = true;
    server.use(
      http.get('*/v1/me', () => HttpResponse.json(meBody(ALICE))),
      http.get('*/v1/projects/:p/executions', () => (revoked ? unauthorized() : empty())),
      ...directory(),
    );
    tokenStore.set('trb_pat_revoked');
    const { router } = renderAt('/p/prod/executions?status=running');
    expect(await screen.findByText(/session ended/i)).toBeInTheDocument();
    expect(router.state.location.pathname).toBe('/sign-in');
    expect(router.state.location.search).toMatchObject({
      reason: 'session-expired',
      next: '/p/prod/executions?status=running',
    });

    revoked = false;
    const user = userEvent.setup();
    await user.type(screen.getByLabelText(/personal access token/i), 'trb_pat_fresh');
    await user.click(screen.getByRole('button', { name: /sign in/i }));
    await waitFor(() => expect(router.state.location.pathname).toBe('/p/prod/executions'));
    expect(router.state.location.search).toMatchObject({ status: 'running' });
  });

  it('a restored token that no longer qualifies keeps the destination too', async () => {
    server.use(
      ...directory(),
      http.get('*/v1/me', () =>
        HttpResponse.json(meBody({ ...ALICE, scopes: ['runbooks:read', 'webhooks:manage'] })),
      ),
    );
    tokenStore.set('trb_pat_wide');
    const { router } = renderAt('/p/prod/executions?status=running');
    await waitFor(() => expect(router.state.location.pathname).toBe('/sign-in'));
    expect(router.state.location.search).toMatchObject({
      reason: 'session-expired',
      next: '/p/prod/executions?status=running',
    });
    expect(tokenStore.get()).toBeNull();
  });
});

describe('ending a session forgets the previous person (U1)', () => {
  it("header sign-out: the next person sees none of the first person's data", async () => {
    const { bobGate, aliceTasksGate } = twoPeople();
    aliceTasksGate.resolve();
    tokenStore.set(ALICE_TOKEN);
    renderAt('/tasks');
    expect(await screen.findByText(ALICE_TASK)).toBeInTheDocument();
    expect(screen.getByText('Alice')).toBeInTheDocument();
    const aliceSession = aliceTabState();

    const user = userEvent.setup();
    // UI-1: sign-out lives in the account menu at the sidebar's foot.
    await user.click(screen.getByRole('button', { name: /^account:/i }));
    await user.click(await screen.findByRole('button', { name: /sign out/i }));
    expect(await screen.findByText(/have signed out/i)).toBeInTheDocument();
    expect(runbookKeys()).toEqual([]);
    expect(tabClaims.all()).toEqual({});

    await signInAsBobAndLook(bobGate);
    await expectFreshTabSession(aliceSession);
  });

  it("a 401: the next person sees none of the first person's data", async () => {
    const { bobGate, aliceTasksGate } = twoPeople({ aliceTasks: unauthorized });
    tokenStore.set(ALICE_TOKEN);
    renderAt('/tasks');
    expect(await screen.findByText('Alice')).toBeInTheDocument();
    const aliceSession = aliceTabState();
    aliceTasksGate.resolve();
    expect(await screen.findByText(/session ended/i)).toBeInTheDocument();
    expect(tokenStore.get()).toBeNull();
    expect(runbookKeys()).toEqual([]);
    expect(tabClaims.all()).toEqual({});

    await signInAsBobAndLook(bobGate);
    await expectFreshTabSession(aliceSession);
  });

  it('a refused restored token: the next person sees none of its data', async () => {
    const bobGate = deferred();
    let bobMeCalls = 0;
    server.use(
      http.get('*/v1/me', async ({ request }) => {
        if (request.headers.get('authorization') === `Bearer ${BOB_TOKEN}`) {
          bobMeCalls += 1;
          if (bobMeCalls > 1) await bobGate.promise;
          return HttpResponse.json(meBody(BOB));
        }
        return HttpResponse.json(meBody({ ...ALICE, scopes: ['webhooks:manage'] }));
      }),
      http.get('*/v1/me/tasks', async () => {
        await bobGate.promise;
        return empty();
      }),
      ...directory(),
    );
    aliceTabState();
    tokenStore.set('trb_pat_wide');
    renderAt('/tasks');
    expect(await screen.findByText(/session ended/i)).toBeInTheDocument();
    expect(runbookKeys()).toEqual([]);
    expect(tabClaims.all()).toEqual({});

    const user = userEvent.setup();
    await user.type(screen.getByLabelText(/personal access token/i), BOB_TOKEN);
    await user.click(screen.getByRole('button', { name: /sign in/i }));
    expect(await screen.findByText(/loading your session/i)).toBeInTheDocument();
    expect(screen.queryByText(/no longer qualifies/i)).toBeNull();
    bobGate.resolve();
    expect(await screen.findByText('Bob')).toBeInTheDocument();
  });
});
