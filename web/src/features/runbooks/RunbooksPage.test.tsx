// UI-1: the runbooks list: server search and tag filters, last-run filter,
// row actions, keyboard movement, and the empty state's two ways in.
// (U20 before it: each row reaches the runbook's runs and versions.)
import { createMemoryHistory } from '@tanstack/react-router';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { App } from '../../App';
import { tokenStore } from '../../api/token-store';
import { createAppRouter } from '../../routes/router';
import { expectNoAxeViolations } from '../../test/axe';
import { server } from '../../test/msw/server';
import type { RunbookRow } from './catalog';

afterEach(() => tokenStore.clear());

const NOW = Date.now();

const ROWS: RunbookRow[] = [
  {
    runbook_id: 'rb_1',
    slug: 'post-deploy',
    origin: 'ui',
    archived: false,
    title: 'Post-deploy checks',
    description: 'Health, error rates and latency after every deploy.',
    tags: ['deploy', 'agentic'],
    latest_version: 3,
    draft_ahead: true,
    last_run: { id: 'exe_failed', status: 'failed', created_at_ms: NOW - 3_600_000 },
    active_runs: 2,
  },
  {
    runbook_id: 'rb_2',
    slug: 'customer-onboarding',
    origin: 'git',
    archived: false,
    title: 'Customer onboarding',
    tags: ['customers'],
    latest_version: 1,
    draft_ahead: false,
    last_run: { id: 'exe_waiting', status: 'waiting', created_at_ms: NOW - 60_000 },
    active_runs: 1,
  },
  {
    runbook_id: 'rb_3',
    slug: 'cert-rotation',
    origin: 'ui',
    archived: false,
    title: 'Certificate rotation',
    tags: [],
    draft_ahead: false,
    active_runs: 0,
  },
];

function shellHandlers(scopes: string[]) {
  return [
    http.get('*/v1/me', () =>
      HttpResponse.json({
        user: { id: 'usr_1', name: 'Alice' },
        org: { id: 'org_1', title: 'Acme', role: 'owner' },
        teams: [],
        credential: {
          kind: 'personal',
          actor_kind: 'human',
          scopes,
          projects: null,
          expires_at_ms: Date.now() + 3_600_000,
        },
      }),
    ),
    http.get('*/v1/orgs', () =>
      HttpResponse.json({ items: [{ id: 'org_1', title: 'Acme' }], next_cursor: null }),
    ),
    http.get('*/v1/orgs/:org/projects', () =>
      HttpResponse.json({
        items: [{ id: 'prj_1', org_id: 'org_1', slug: 'prod', title: 'Production' }],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/orgs/:org/users', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/orgs/:org/teams', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/me/tasks', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/projects/:p/executions', () =>
      HttpResponse.json({ items: [], next_cursor: null }),
    ),
  ];
}

/** The server's reading of `tag`: comma-separated, `\,` and `\\` escaped. */
function parseTagParam(value: string | null): string[] {
  if (value == null) return [];
  const tags: string[] = [];
  let current = '';
  for (let i = 0; i < value.length; i++) {
    const c = value[i]!;
    if (c === '\\' && i + 1 < value.length) current += value[++i];
    else if (c === ',') {
      tags.push(current);
      current = '';
    } else current += c;
  }
  tags.push(current);
  return tags;
}

/** Serves ROWS, honouring `q` and `tag` like the server; records each query. */
function catalogHandler(
  rows: RunbookRow[],
  seen: URLSearchParams[] = [],
  nextCursor: string | null = null,
) {
  return http.get('*/v1/projects/:p/runbooks', ({ request }) => {
    const params = new URL(request.url).searchParams;
    seen.push(params);
    const q = params.get('q')?.toLowerCase();
    const tags = parseTagParam(params.get('tag'));
    const items = rows.filter(
      (row) =>
        (!q ||
          [row.title, row.slug, row.description, ...(row.tags ?? [])].some((v) =>
            v?.toLowerCase().includes(q),
          )) &&
        tags.every((t) => row.tags?.includes(t)),
    );
    return HttpResponse.json({ items, next_cursor: params.get('cursor') ? null : nextCursor });
  });
}

function renderList(
  rows: RunbookRow[],
  {
    scopes = ['runbooks:read', 'runbooks:write'],
    seen = [] as URLSearchParams[],
    nextCursor = null as string | null,
  } = {},
) {
  server.use(
    ...shellHandlers(scopes),
    catalogHandler(rows, seen, nextCursor),
    // UI-1c: one runbook by slug (the new-runbook form's taken-ID check).
    http.get('*/v1/projects/:p/runbooks/:slug', ({ params }) => {
      const row = rows.find((r) => r.slug === params.slug);
      return row
        ? HttpResponse.json(row)
        : HttpResponse.json({ code: 'resource.not_found', status: 404 }, { status: 404 });
    }),
  );
  tokenStore.set('trb_pat_good');
  const router = createAppRouter(createMemoryHistory({ initialEntries: ['/p/prod/runbooks'] }));
  render(<App router={router} />);
  return router;
}

const rowOf = (name: string) => screen.getByRole('link', { name }).closest('li') as HTMLElement;

describe('runbooks list (UI-1)', () => {
  it('shows each runbook with its last run, version, draft marker, origin and tags', async () => {
    renderList(ROWS);
    await screen.findByRole('link', { name: 'Post-deploy checks' });
    const post = rowOf('Post-deploy checks');
    expect(within(post).getByText('post-deploy')).toBeInTheDocument();
    expect(
      within(post).getByText('Health, error rates and latency after every deploy.'),
    ).toBeInTheDocument();
    expect(within(post).getByText('v3')).toBeInTheDocument();
    expect(within(post).getByText('draft ahead')).toBeInTheDocument();
    expect(within(post).getByText('2 active')).toBeInTheDocument();
    expect(within(post).getByRole('link', { name: /last run failed/i })).toHaveAttribute(
      'href',
      '/x/exe_failed',
    );
    expect(
      within(within(post).getByRole('list', { name: 'Tags' })).getAllByRole('listitem'),
    ).toHaveLength(2);

    const onboarding = rowOf('Customer onboarding');
    expect(within(onboarding).getByText('git')).toBeInTheDocument();
    expect(within(onboarding).queryByText('draft ahead')).not.toBeInTheDocument();

    const certs = rowOf('Certificate rotation');
    expect(within(certs).getByText('Never run')).toBeInTheDocument();
    expect(within(certs).getByText('unpublished')).toBeInTheDocument();
    // Nothing published: nothing to start.
    expect(
      within(certs).getByRole('button', { name: /start a run of certificate rotation/i }),
    ).toBeDisabled();
    await expectNoAxeViolations(screen.getByRole('main'));
  });

  it('opens the runbook, edits its draft and reaches its versions (U20)', async () => {
    const user = userEvent.setup();
    const router = renderList(ROWS);
    await screen.findByRole('link', { name: 'Post-deploy checks' });
    const post = rowOf('Post-deploy checks');
    expect(within(post).getByRole('link', { name: 'Post-deploy checks' })).toHaveAttribute(
      'href',
      // UI-2: the runbook's own page (graph first).
      '/p/prod/runbooks/rb_1',
    );
    expect(
      within(post).getByRole('link', { name: /edit the draft of post-deploy checks/i }),
    ).toHaveAttribute('href', '/p/prod/runbooks/rb_1/editor');
    // A Git-managed runbook is viewed, not edited, here.
    expect(
      within(rowOf('Customer onboarding')).getByRole('link', {
        name: /view the source of customer onboarding/i,
      }),
    ).toBeInTheDocument();
    await user.click(
      within(post).getByRole('button', { name: /more actions for post-deploy checks/i }),
    );
    await user.click(await screen.findByRole('menuitem', { name: 'Versions' }));
    await waitFor(() =>
      expect(router.state.location.pathname).toBe('/p/prod/runbooks/rb_1/versions'),
    );
  });

  it('starts a run of the row it was asked from', async () => {
    const user = userEvent.setup();
    renderList(ROWS);
    await screen.findByRole('link', { name: 'Post-deploy checks' });
    await user.click(screen.getByRole('button', { name: /start a run of customer onboarding/i }));
    const dialog = await screen.findByRole('dialog', { name: 'Start a run' });
    expect(dialog).toBeInTheDocument();
  });

  it('searches on the server, debounced', async () => {
    const user = userEvent.setup();
    const seen: URLSearchParams[] = [];
    renderList(ROWS, { seen });
    await screen.findByRole('link', { name: 'Post-deploy checks' });
    await user.type(screen.getByRole('searchbox', { name: 'Search runbooks' }), 'onboard');
    await waitFor(() =>
      expect(screen.queryByRole('link', { name: 'Post-deploy checks' })).not.toBeInTheDocument(),
    );
    expect(screen.getByRole('link', { name: 'Customer onboarding' })).toBeInTheDocument();
    const queries = seen.map((p) => p.get('q')).filter(Boolean);
    // One request for the settled text, not one per keystroke.
    expect(queries).toEqual(['onboard']);
  });

  it('filters by tag on the server', async () => {
    const user = userEvent.setup();
    const seen: URLSearchParams[] = [];
    renderList(ROWS, { seen });
    await screen.findByRole('link', { name: 'Post-deploy checks' });
    await user.click(screen.getByRole('button', { name: 'Filter by tag' }));
    await user.click(await screen.findByRole('menuitemcheckbox', { name: 'customers' }));
    await user.keyboard('{Escape}');
    await waitFor(() =>
      expect(screen.queryByRole('link', { name: 'Post-deploy checks' })).not.toBeInTheDocument(),
    );
    expect(seen.at(-1)?.get('tag')).toBe('customers');
    await user.click(screen.getByRole('button', { name: 'Remove tag filter customers' }));
    expect(await screen.findByRole('link', { name: 'Post-deploy checks' })).toBeInTheDocument();
  });

  it('filters by last run with counts, and says when nothing matches', async () => {
    const user = userEvent.setup();
    renderList(ROWS);
    await screen.findByRole('link', { name: 'Post-deploy checks' });
    const group = screen.getByRole('group', { name: 'Filter by last run' });
    await user.click(within(group).getByRole('button', { name: /failing\s*1/i }));
    expect(screen.getByRole('link', { name: 'Post-deploy checks' })).toBeInTheDocument();
    expect(screen.queryByRole('link', { name: 'Customer onboarding' })).not.toBeInTheDocument();
    await user.click(within(group).getByRole('button', { name: /needs a person\s*1/i }));
    expect(screen.getByRole('link', { name: 'Customer onboarding' })).toBeInTheDocument();
    await user.click(within(group).getByRole('button', { name: /running\s*0/i }));
    expect(screen.getByText('No runbooks match')).toBeInTheDocument();
    await user.click(screen.getAllByRole('button', { name: 'Clear filters' })[0]!);
    expect(
      screen.getAllByRole('link', { name: /checks|onboarding|rotation/i }).length,
    ).toBeGreaterThanOrEqual(3);
  });

  it('moves between rows with the arrow keys', async () => {
    renderList(ROWS);
    const first = await screen.findByRole('link', { name: 'Post-deploy checks' });
    first.focus();
    fireEvent.keyDown(first, { key: 'ArrowDown' });
    expect(screen.getByRole('link', { name: 'Customer onboarding' })).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: 'End' });
    expect(screen.getByRole('link', { name: 'Certificate rotation' })).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: 'k' });
    expect(screen.getByRole('link', { name: 'Customer onboarding' })).toHaveFocus();
  });

  it('offers a form and the agent prompt when the project has no runbooks', async () => {
    renderList([]);
    expect(await screen.findByRole('heading', { name: 'No runbooks yet' })).toBeInTheDocument();
    const form = screen.getByRole('form', { name: 'New runbook' });
    expect(
      within(form).getByRole('button', { name: 'Continue to the builder' }),
    ).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Create with your AI agent' })).toBeInTheDocument();
    expect(screen.getByLabelText('CLI commands')).toHaveTextContent(
      'runbook push post-deploy.yaml --draft --project prod',
    );
    await expectNoAxeViolations(screen.getByRole('main'));
  });

  it('opens the builder with an unsaved draft from the empty state (UI-4)', async () => {
    const user = userEvent.setup();
    let created = false;
    server.use(
      http.post('*/v1/projects/:p/runbooks', () => {
        created = true;
        return HttpResponse.json({}, { status: 500 });
      }),
    );
    const router = renderList([]);
    const form = await screen.findByRole('form', { name: 'New runbook' });
    await user.type(within(form).getByLabelText('Name'), 'DB failover');
    expect(within(form).getByLabelText('ID')).toHaveValue('db-failover');
    await user.click(within(form).getByRole('button', { name: 'Continue to the builder' }));
    await waitFor(() => expect(router.state.location.pathname).toBe('/p/prod/runbooks/new'));
    expect(router.state.location.search).toEqual({
      template: 'blank',
      name: 'DB failover',
      id: 'db-failover',
    });
    // Nothing is created until the builder saves.
    expect(created).toBe(false);
  });

  it('shows the empty state without the form to a reader', async () => {
    renderList([], { scopes: ['runbooks:read'] });
    expect(await screen.findByRole('heading', { name: 'No runbooks yet' })).toBeInTheDocument();
    expect(screen.queryByRole('form', { name: 'New runbook' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'New runbook' })).not.toBeInTheDocument();
  });

  it('explains a failed load and retries', async () => {
    // Failing until the person retries (the query itself retries a 5xx once, U15).
    let failing = true;
    server.use(
      ...shellHandlers(['runbooks:read']),
      http.get('*/v1/projects/:p/runbooks', () =>
        failing
          ? HttpResponse.json(
              { status: 503, code: 'storage.unavailable', title: 'Unavailable' },
              { status: 503, headers: { 'content-type': 'application/problem+json' } },
            )
          : HttpResponse.json({ items: ROWS, next_cursor: null }),
      ),
    );
    tokenStore.set('trb_pat_good');
    const router = createAppRouter(createMemoryHistory({ initialEntries: ['/p/prod/runbooks'] }));
    render(<App router={router} />);
    const alert = await screen.findByRole('alert', {}, { timeout: 4000 });
    expect(alert).toHaveTextContent('Runbooks could not be loaded.');
    failing = false;
    await userEvent.setup().click(within(alert).getByRole('button', { name: 'Retry' }));
    expect(await screen.findByRole('link', { name: 'Post-deploy checks' })).toBeInTheDocument();
  });

  // Without executions:read the server omits last_run and active_runs.
  it('shows a dash, not "Never run", when run data is withheld, and hides run-based controls', async () => {
    const user = userEvent.setup();
    const withheld = ROWS.map(({ last_run: _l, active_runs: _a, ...row }) => row);
    renderList(withheld as RunbookRow[]);
    await screen.findByRole('link', { name: 'Post-deploy checks' });
    const post = rowOf('Post-deploy checks');
    expect(within(post).queryByText('Never run')).not.toBeInTheDocument();
    expect(within(post).queryByText(/active/)).not.toBeInTheDocument();
    expect(within(post).getByText('—')).toBeInTheDocument();
    expect(within(post).getByText('Run history needs executions:read')).toBeInTheDocument();
    expect(within(rowOf('Certificate rotation')).queryByText('Never run')).not.toBeInTheDocument();
    expect(screen.queryByRole('group', { name: 'Filter by last run' })).not.toBeInTheDocument();
    await user.click(screen.getByRole('combobox', { name: 'Sort runbooks' }));
    const options = (await screen.findAllByRole('option')).map((o) => o.textContent);
    expect(options).toEqual(['Newest first', 'Name']);
    await user.keyboard('{Escape}');
    await expectNoAxeViolations(screen.getByRole('main'));
  });

  it('keeps the search box when a search fails, and retries the current text', async () => {
    const user = userEvent.setup();
    const seen: URLSearchParams[] = [];
    server.use(
      ...shellHandlers(['runbooks:read']),
      http.get('*/v1/projects/:p/runbooks', ({ request }) => {
        const params = new URL(request.url).searchParams;
        if (params.get('limit') === '25') seen.push(params);
        return params.get('q')?.startsWith('boom')
          ? HttpResponse.json(
              { status: 400, code: 'request.invalid', title: 'Bad search' },
              { status: 400, headers: { 'content-type': 'application/problem+json' } },
            )
          : HttpResponse.json({ items: ROWS, next_cursor: null });
      }),
    );
    tokenStore.set('trb_pat_good');
    render(
      <App
        router={createAppRouter(createMemoryHistory({ initialEntries: ['/p/prod/runbooks'] }))}
      />,
    );
    await screen.findByRole('link', { name: 'Post-deploy checks' });
    const search = screen.getByRole('searchbox', { name: 'Search runbooks' });
    await user.type(search, 'boom');
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('Runbooks could not be loaded.');
    // The search box (and the rest of the toolbar) stays, with the text.
    expect(screen.getByRole('searchbox', { name: 'Search runbooks' })).toHaveValue('boom');
    expect(screen.getByRole('button', { name: 'Filter by tag' })).toBeInTheDocument();
    // Retry right after typing more asks for what the box says now.
    await user.type(screen.getByRole('searchbox', { name: 'Search runbooks' }), 'x');
    const before = seen.length;
    await user.click(within(alert).getByRole('button', { name: 'Retry' }));
    await waitFor(() => expect(seen.at(-1)?.get('q')).toBe('boomx'));
    // Not the stale text the debounce had not caught up with.
    expect(seen.slice(before).map((p) => p.get('q'))).toEqual(['boomx']);
    // Changing the search asks the server again, and the rows come back.
    await user.clear(screen.getByRole('searchbox', { name: 'Search runbooks' }));
    await user.type(screen.getByRole('searchbox', { name: 'Search runbooks' }), 'post');
    expect(await screen.findByRole('link', { name: 'Post-deploy checks' })).toBeInTheDocument();
    expect(seen.at(-1)?.get('q')).toBe('post');
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    // Clearing it shows the whole list again.
    await user.clear(screen.getByRole('searchbox', { name: 'Search runbooks' }));
    expect(await screen.findByRole('link', { name: 'Customer onboarding' })).toBeInTheDocument();
  });

  it('says the last-run counts cover only the loaded rows while more exist', async () => {
    renderList(ROWS, { nextCursor: 'page2' });
    await screen.findByRole('link', { name: 'Post-deploy checks' });
    const group = screen.getByRole('group', { name: 'Filter by last run' });
    const note = screen.getByText(/counts cover the 3 runbooks loaded so far/i);
    expect(note).toBeVisible();
    expect(group).toHaveAccessibleDescription(
      expect.stringMatching(/counts cover the 3 runbooks loaded so far/i),
    );
  });

  it('follows a short search page the server could not finish (UI-1b review round 1)', async () => {
    // Rows the server has not listed yet are examined a bounded number a
    // request; a search past that answers a short page and a cursor.
    const seen: URLSearchParams[] = [];
    server.use(
      ...shellHandlers(['runbooks:read']),
      http.get('*/v1/projects/:p/runbooks', ({ request }) => {
        const params = new URL(request.url).searchParams;
        seen.push(params);
        if (!params.get('q')) return HttpResponse.json({ items: ROWS, next_cursor: null });
        const cursor = params.get('cursor');
        if (!cursor) return HttpResponse.json({ items: [], next_cursor: 'c1' });
        if (cursor === 'c1') return HttpResponse.json({ items: [], next_cursor: 'c2' });
        return HttpResponse.json({ items: [ROWS[2]], next_cursor: null });
      }),
    );
    tokenStore.set('trb_pat_good');
    const router = createAppRouter(createMemoryHistory({ initialEntries: ['/p/prod/runbooks'] }));
    render(<App router={router} />);
    const user = userEvent.setup();
    await screen.findByRole('link', { name: 'Post-deploy checks' });
    await user.type(screen.getByRole('searchbox', { name: 'Search runbooks' }), 'rotation');
    await waitFor(() =>
      expect(screen.queryByRole('link', { name: 'Post-deploy checks' })).not.toBeInTheDocument(),
    );
    expect(screen.getByRole('link', { name: 'Certificate rotation' })).toBeInTheDocument();
    expect(screen.queryByText('No runbooks match')).not.toBeInTheDocument();
    expect(seen.filter((p) => p.get('q')).map((p) => p.get('cursor'))).toEqual([null, 'c1', 'c2']);
  });

  it('cancels an old search’s cursor follow when the search changes (UI-1c)', async () => {
    const aborted: string[] = [];
    let followsStarted = 0;
    server.use(
      ...shellHandlers(['runbooks:read']),
      http.get('*/v1/projects/:p/runbooks', async ({ request }) => {
        const params = new URL(request.url).searchParams;
        const q = params.get('q');
        if (!q) return HttpResponse.json({ items: ROWS, next_cursor: null });
        if (!params.get('cursor')) return HttpResponse.json({ items: [], next_cursor: 'c1' });
        // The follow of an old search: answers only when cancelled.
        followsStarted += 1;
        await new Promise<void>((resolve) =>
          request.signal.addEventListener('abort', () => {
            aborted.push(q);
            resolve();
          }),
        );
        return HttpResponse.json({ items: [], next_cursor: null });
      }),
    );
    tokenStore.set('trb_pat_good');
    const router = createAppRouter(createMemoryHistory({ initialEntries: ['/p/prod/runbooks'] }));
    render(<App router={router} />);
    const user = userEvent.setup();
    await screen.findByRole('link', { name: 'Post-deploy checks' });
    const box = screen.getByRole('searchbox', { name: 'Search runbooks' });
    await user.type(box, 'rot');
    await waitFor(() => expect(followsStarted).toBe(1));
    await user.type(box, 'ation');
    await waitFor(() => expect(aborted).toContain('rot'));
  });

  it('says a search found nothing so far while the server has more to examine', async () => {
    server.use(
      ...shellHandlers(['runbooks:read']),
      http.get('*/v1/projects/:p/runbooks', ({ request }) => {
        const params = new URL(request.url).searchParams;
        if (!params.get('q')) return HttpResponse.json({ items: ROWS, next_cursor: null });
        const n = Number(params.get('cursor') ?? '0');
        return HttpResponse.json({ items: [], next_cursor: String(n + 1) });
      }),
    );
    tokenStore.set('trb_pat_good');
    const router = createAppRouter(createMemoryHistory({ initialEntries: ['/p/prod/runbooks'] }));
    render(<App router={router} />);
    const user = userEvent.setup();
    await screen.findByRole('link', { name: 'Post-deploy checks' });
    await user.type(screen.getByRole('searchbox', { name: 'Search runbooks' }), 'zzz');
    expect(
      await screen.findByText(/Nothing matches “zzz” among the runbooks searched so far/),
    ).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Load more' })).toBeInTheDocument();
  });

  it('does not say so when every row is loaded', async () => {
    renderList(ROWS);
    await screen.findByRole('link', { name: 'Post-deploy checks' });
    expect(screen.queryByText(/counts cover/i)).not.toBeInTheDocument();
  });

  it('shows each tag once, without duplicate keys', async () => {
    const errors = vi.spyOn(console, 'error');
    const user = userEvent.setup();
    renderList([{ ...ROWS[0]!, tags: ['deploy', 'deploy', 'agentic'] }]);
    await screen.findByRole('link', { name: 'Post-deploy checks' });
    const tags = within(rowOf('Post-deploy checks')).getByRole('list', { name: 'Tags' });
    expect(
      within(tags)
        .getAllByRole('listitem')
        .map((li) => li.textContent),
    ).toEqual(['deploy', 'agentic']);
    await user.click(screen.getByRole('button', { name: 'Filter by tag' }));
    const options = await screen.findAllByRole('menuitemcheckbox');
    expect(options.map((o) => o.textContent)).toEqual(['agentic', 'deploy']);
    expect(errors.mock.calls.flat().join(' ')).not.toMatch(/same key/);
  });

  it('escapes a comma or backslash inside a tag filter', async () => {
    const user = userEvent.setup();
    const seen: URLSearchParams[] = [];
    renderList([{ ...ROWS[0]!, tags: ['a,b', 'c\\d'] }, ROWS[1]!], { seen });
    await screen.findByRole('link', { name: 'Post-deploy checks' });
    await user.click(screen.getByRole('button', { name: 'Filter by tag' }));
    await user.click(await screen.findByRole('menuitemcheckbox', { name: 'a,b' }));
    await user.click(await screen.findByRole('menuitemcheckbox', { name: 'c\\d' }));
    await user.keyboard('{Escape}');
    await waitFor(() => expect(seen.at(-1)?.get('tag')).toBe('a\\,b,c\\\\d'));
    await waitFor(() =>
      expect(screen.queryByRole('link', { name: 'Customer onboarding' })).not.toBeInTheDocument(),
    );
    expect(screen.getByRole('link', { name: 'Post-deploy checks' })).toBeInTheDocument();
  });

  it('marks a description the server cut short', async () => {
    renderList([
      { ...ROWS[0]!, description: 'Health, error rates', description_truncated: true },
      ROWS[1]!,
    ]);
    await screen.findByRole('link', { name: 'Post-deploy checks' });
    expect(
      within(rowOf('Post-deploy checks')).getByText('Health, error rates…'),
    ).toBeInTheDocument();
  });

  // With keepPreviousData, a cleared search whose unfiltered list is not in
  // the cache (here: its first load failed) shows the last search's zero rows
  // as placeholder data until the list arrives: not an empty project.
  it('never flashes the empty-project state while a cleared search reloads', async () => {
    const user = userEvent.setup();
    let release: () => void = () => {};
    let unfiltered = 0;
    server.use(
      ...shellHandlers(['runbooks:read']),
      http.get('*/v1/projects/:p/runbooks', async ({ request }) => {
        const params = new URL(request.url).searchParams;
        // Only this page's list (25 a page) is held; the shell loads its own.
        if (params.get('limit') !== '25')
          return HttpResponse.json({ items: ROWS, next_cursor: null });
        if (params.get('q')) return HttpResponse.json({ items: [], next_cursor: null });
        unfiltered += 1;
        if (unfiltered === 1)
          return HttpResponse.json(
            { status: 400, code: 'request.invalid', title: 'Bad request' },
            { status: 400, headers: { 'content-type': 'application/problem+json' } },
          );
        await new Promise<void>((resolve) => (release = resolve));
        return HttpResponse.json({ items: ROWS, next_cursor: null });
      }),
    );
    tokenStore.set('trb_pat_good');
    render(
      <App
        router={createAppRouter(createMemoryHistory({ initialEntries: ['/p/prod/runbooks'] }))}
      />,
    );
    await screen.findByRole('alert');
    await user.type(screen.getByRole('searchbox', { name: 'Search runbooks' }), 'zzz');
    expect(await screen.findByText('No runbooks match')).toBeInTheDocument();
    await user.clear(screen.getByRole('searchbox', { name: 'Search runbooks' }));
    await waitFor(() => expect(unfiltered).toBe(2));
    expect(screen.queryByRole('heading', { name: 'No runbooks yet' })).not.toBeInTheDocument();
    expect(screen.getByRole('searchbox', { name: 'Search runbooks' })).toBeInTheDocument();
    release();
    expect(await screen.findByRole('link', { name: 'Post-deploy checks' })).toBeInTheDocument();
    expect(screen.queryByRole('heading', { name: 'No runbooks yet' })).not.toBeInTheDocument();
  });

  // In a browser "/" would move focus out of the open menu (and its focus
  // trap fights back); here, the shortcut must leave the key to the menu:
  // not take it (preventDefault) and not focus the search box.
  it('focuses search on "/" but not from an open menu or list', async () => {
    const user = userEvent.setup();
    renderList(ROWS);
    await screen.findByRole('link', { name: 'Post-deploy checks' });
    const search = screen.getByRole('searchbox', { name: 'Search runbooks' });
    await user.keyboard('/');
    expect(search).toHaveFocus();
    search.blur();
    let focused = 0;
    search.addEventListener('focus', () => (focused += 1));
    /** Presses "/" where focus is; true when nothing took the key. */
    const slash = () => fireEvent.keyDown(document.activeElement ?? document.body, { key: '/' });

    await user.click(screen.getByRole('button', { name: 'Filter by tag' }));
    const menu = await screen.findByRole('menu');
    await waitFor(() => expect(menu).toContainElement(document.activeElement as HTMLElement));
    expect(slash()).toBe(true);
    await user.keyboard('{ArrowDown}');
    expect(document.activeElement).toHaveAttribute('role', 'menuitemcheckbox');
    expect(slash()).toBe(true);
    await user.keyboard('{Escape}');

    await user.click(
      within(rowOf('Post-deploy checks')).getByRole('button', { name: /more actions/i }),
    );
    await screen.findByRole('menuitem', { name: 'Versions' });
    expect(slash()).toBe(true);
    await user.keyboard('{Escape}');

    await user.click(screen.getByRole('combobox', { name: 'Sort runbooks' }));
    await screen.findByRole('listbox');
    expect(slash()).toBe(true);
    expect(focused).toBe(0);
  });

  it('leaves "/" alone when something else already handled it', async () => {
    renderList(ROWS);
    const link = await screen.findByRole('link', { name: 'Post-deploy checks' });
    link.addEventListener('keydown', (event) => event.preventDefault());
    link.focus();
    fireEvent.keyDown(link, { key: '/' });
    expect(link).toHaveFocus();
  });
});
