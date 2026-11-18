// The creation flow (UI-4) through the app: "New runbook" offers blank, a
// template or the AI agent; blank and template open the builder with an
// unsaved draft; its first save creates the runbook (D23 and catalog
// refusals in plain words, one Idempotency-Key per attempt) and the editor
// moves to the new runbook's URL. Nothing is created before that save.
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
import {
  EDITOR_SCOPES,
  RB,
  editorHandlers,
  etagOf,
  problem,
  type EditorServer,
} from './test-support';

vi.mock('./YamlEditor', async () => ({
  default: (await import('./test-support')).StubYamlEditor,
}));

afterEach(() => tokenStore.clear());

function renderRunbooks(scopes: string[], catalog: EditorServer['catalog'] = []) {
  server.use(
    ...editorHandlers({ draft: { source: '', revision: 1 }, latestVersion: null, scopes, catalog }),
    http.get('*/v1/me/tasks', () => HttpResponse.json({ items: [], next_cursor: null })),
  );
  tokenStore.set('trb_pat_test');
  const router = createAppRouter(createMemoryHistory({ initialEntries: ['/p/prod/runbooks'] }));
  render(<App router={router} />);
  return router;
}

type Post = { key: string | null; body: { source: string; slug: string | null } };

function createHandler(posts: Post[], answers: Response[] = []) {
  return http.post('*/v1/projects/:p/runbooks', async ({ request }) => {
    posts.push({
      key: request.headers.get('idempotency-key'),
      body: (await request.json()) as Post['body'],
    });
    const answer = answers.shift();
    if (answer) return answer;
    return HttpResponse.json(
      { runbook_id: RB, source: posts.at(-1)?.body.source, revision: 1, diagnostics: [] },
      { status: 201, headers: { ETag: etagOf(1) } },
    );
  });
}

async function openDialog() {
  const user = userEvent.setup();
  await user.click(await screen.findByRole('button', { name: 'New runbook' }));
  const dialog = await screen.findByRole('dialog', { name: 'New runbook' });
  return { user, dialog };
}

describe('creating a runbook', () => {
  it('opens a template in the builder unsaved; the first save creates the draft', async () => {
    const posts: Post[] = [];
    server.use(createHandler(posts, [problem(409, 'catalog.runbook_slug_conflict')]));
    const router = renderRunbooks(EDITOR_SCOPES);
    const { user, dialog } = await openDialog();
    await expectNoAxeViolations(dialog);

    await user.click(within(dialog).getByRole('radio', { name: /From a template/ }));
    expect(within(dialog).getByRole('radio', { name: /Post-deploy checks/ })).toBeChecked();
    expect(within(dialog).getByLabelText('Name')).toHaveValue('Post-deploy checks');
    expect(within(dialog).getByLabelText('ID')).toHaveValue('post-deploy-checks');
    await user.click(within(dialog).getByRole('radio', { name: /Customer onboarding/ }));
    expect(within(dialog).getByLabelText('Name')).toHaveValue('Customer onboarding');
    await user.click(within(dialog).getByRole('radio', { name: /Post-deploy checks/ }));
    await expectNoAxeViolations(dialog);
    await user.click(within(dialog).getByRole('button', { name: 'Continue to the builder' }));

    // The builder, with the template's steps, and nothing saved yet.
    expect(await screen.findByRole('heading', { level: 1, name: 'New runbook' })).toBeVisible();
    expect(router.state.location.pathname).toBe('/p/prod/runbooks/new');
    expect(
      await screen.findByText(/Not saved yet\. Saving creates the runbook as a draft/),
    ).toBeVisible();
    await waitFor(() => expect(document.querySelector('[data-node-id="health"]')).not.toBeNull());
    expect(posts).toHaveLength(0);
    expect(screen.queryByRole('button', { name: 'Publish' })).not.toBeInTheDocument();

    // A refusal is explained; the next attempt is a new action.
    await user.click(screen.getByRole('button', { name: 'Save draft' }));
    expect(await screen.findByTestId('editor-notice')).toHaveTextContent(
      'A runbook with that slug already exists in this project.',
    );
    await user.click(screen.getByRole('button', { name: 'Save draft' }));
    expect(await screen.findByRole('heading', { level: 1, name: 'Editor' })).toBeVisible();
    await waitFor(() =>
      expect(router.state.location.pathname).toBe(`/p/prod/runbooks/${RB}/editor`),
    );
    expect(await screen.findByTestId('editor-notice')).toHaveTextContent(
      'Created as a draft (revision 1). The server found no problems. Publish when it is ready.',
    );
    expect(screen.getByRole('button', { name: 'Publish' })).toBeEnabled();

    expect(posts.map((p) => p.body.slug)).toEqual(['post-deploy-checks', 'post-deploy-checks']);
    expect(posts[1]?.body.source).toContain('  id: post-deploy-checks\n');
    expect(posts[1]?.body.source).toContain('  title: Post-deploy checks\n');
    expect(posts[1]?.body.source).toContain('defaults: { actor: agent');
    expect(new Set(posts.map((p) => p.key)).size).toBe(2);
  });

  it('retries an unanswered create with the same key', async () => {
    const posts: Post[] = [];
    server.use(createHandler(posts, [problem(503, 'storage.unavailable')]));
    renderRunbooks(EDITOR_SCOPES);
    const { user, dialog } = await openDialog();
    await user.type(within(dialog).getByLabelText('Name'), 'Cache warmup');
    await user.click(within(dialog).getByRole('button', { name: 'Continue to the builder' }));
    await screen.findByRole('heading', { level: 1, name: 'New runbook' });
    await user.click(screen.getByRole('button', { name: 'Save draft' }));
    expect(await screen.findByTestId('editor-notice')).toHaveTextContent(
      'The runbook could not be created (storage.unavailable).',
    );
    await user.click(screen.getByRole('button', { name: 'Save draft' }));
    expect(await screen.findByRole('heading', { level: 1, name: 'Editor' })).toBeVisible();
    expect(posts).toHaveLength(2);
    expect(posts[1]?.key).toBe(posts[0]?.key);
    expect(posts[0]?.body.source).toContain('  id: cache-warmup\n');
    expect(posts[0]?.body.source).toContain('  steps: []\n');
  });

  it('starts blank from a name alone, the ID from the name and editable', async () => {
    const router = renderRunbooks(EDITOR_SCOPES, [
      {
        runbook_id: 'rb_taken',
        slug: 'db-failover',
        title: 'DB failover',
        origin: 'ui',
        archived: false,
        latest_version: 1,
        tags: [],
        draft_ahead: false,
      },
    ]);
    const { user, dialog } = await openDialog();
    const continueButton = within(dialog).getByRole('button', { name: 'Continue to the builder' });
    await user.click(continueButton);
    expect(within(dialog).getByRole('alert')).toHaveTextContent('Give the runbook a name.');
    expect(within(dialog).getByLabelText('Name')).toHaveAccessibleDescription(
      'Give the runbook a name.',
    );
    await user.type(within(dialog).getByLabelText('Name'), 'DB failover');
    await waitFor(() =>
      expect(within(dialog).getByRole('alert')).toHaveTextContent(
        'A runbook with this ID already exists in this project.',
      ),
    );
    const id = within(dialog).getByLabelText('ID');
    await user.clear(id);
    await user.type(id, 'db-failover-eu');
    await user.click(continueButton);
    await screen.findByRole('heading', { level: 1, name: 'New runbook' });
    expect(router.state.location.search).toEqual({
      template: 'blank',
      name: 'DB failover',
      id: 'db-failover-eu',
    });
    // A blank runbook: the graph offers the first step.
    expect(await screen.findByRole('button', { name: 'Add the first step' })).toBeVisible();
  });

  it('keeps the AI agent path: the prompt and commands', async () => {
    renderRunbooks(EDITOR_SCOPES);
    const { user, dialog } = await openDialog();
    await user.click(within(dialog).getByRole('radio', { name: /With your AI agent/ }));
    expect(within(dialog).getByLabelText('CLI commands')).toHaveTextContent(
      'runbook push post-deploy.yaml --draft --project prod',
    );
    expect(within(dialog).getByText(/The agent’s draft appears in the runbook list/)).toBeVisible();
    await expectNoAxeViolations(dialog);
    await user.click(within(dialog).getByRole('button', { name: 'Done' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  });

  it('offers no New runbook action without runbooks:write', async () => {
    renderRunbooks(['runbooks:read', 'executions:read']);
    expect(await screen.findByRole('heading', { level: 1, name: 'Runbooks' })).toBeVisible();
    expect(screen.queryByRole('button', { name: 'New runbook' })).not.toBeInTheDocument();
  });

  // UI-4 review round 1.
  async function openNew(name: string) {
    const { user, dialog } = await openDialog();
    await user.type(within(dialog).getByLabelText('Name'), name);
    await user.click(within(dialog).getByRole('button', { name: 'Continue to the builder' }));
    await screen.findByRole('heading', { level: 1, name: 'New runbook' });
    return user;
  }
  const yamlBox = () =>
    screen.getByRole('textbox', { name: /Runbook YAML/ }) as HTMLTextAreaElement;

  it('leaves an untouched template without asking, and still saves it', async () => {
    const posts: Post[] = [];
    server.use(createHandler(posts));
    const router = renderRunbooks(EDITOR_SCOPES);
    await openNew('Cache warmup');
    expect(screen.getByRole('button', { name: 'Save draft' })).toBeEnabled();
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    try {
      // A blocked navigation never settles: not awaited.
      void router.navigate({ href: '/p/prod/runbooks' });
      await waitFor(() => expect(router.state.location.pathname).toBe('/p/prod/runbooks'));
      expect(confirm).not.toHaveBeenCalled();
    } finally {
      confirm.mockRestore();
    }
  });

  it('asks before leaving a template once it is edited', async () => {
    const router = renderRunbooks(EDITOR_SCOPES);
    await openNew('Cache warmup');
    fireEvent.change(yamlBox(), { target: { value: `${yamlBox().value}# mine\n` } });
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    try {
      void router.navigate({ href: '/p/prod/runbooks' });
      await waitFor(() => expect(confirm).toHaveBeenCalled());
      expect(router.state.location.pathname).toBe('/p/prod/runbooks/new');
    } finally {
      confirm.mockRestore();
    }
  });

  /**
   * A server that keeps D20's replay rule: a key answers what it did the
   * first time, for that body only (another body is
   * request.idempotency_key_reused), per principal. `losePost(n)` and
   * `losePatch(n)` lose the answer to the n-th create or save (1-based)
   * after doing it; `dropPost(n)` loses the n-th create before the server
   * sees it.
   */
  function replayingServer(
    live: EditorServer,
    options: {
      losePost?: (n: number) => boolean;
      losePatch?: (n: number) => boolean;
      dropPost?: (n: number) => boolean;
    } = {},
  ) {
    const posts: Post[] = [];
    const patches: { ifMatch: string | null; source: string }[] = [];
    const replays = new Map<string, { body: string; source: string }>();
    let postCount = 0;
    let patchCount = 0;
    const handlers = [
      ...editorHandlers(live),
      http.get('*/v1/me/tasks', () => HttpResponse.json({ items: [], next_cursor: null })),
      http.post('*/v1/projects/:p/runbooks', async ({ request }) => {
        postCount += 1;
        const body = (await request.json()) as Post['body'];
        const key = request.headers.get('idempotency-key') ?? '';
        if (options.dropPost?.(postCount)) return HttpResponse.error();
        posts.push({ key, body });
        const lost = options.losePost?.(postCount) ?? false;
        const answer = (source: string) =>
          lost
            ? HttpResponse.error()
            : HttpResponse.json(
                { runbook_id: RB, source, revision: 1, diagnostics: [] },
                { status: 201, headers: { ETag: etagOf(1) } },
              );
        const replay = replays.get(key);
        if (replay) {
          if (replay.body !== JSON.stringify(body))
            return problem(409, 'request.idempotency_key_reused');
          return answer(replay.source);
        }
        if ((live.catalog ?? []).some((row) => row.slug === body.slug))
          return problem(409, 'catalog.runbook_slug_conflict');
        live.draft = { source: body.source, revision: 1 };
        live.catalog = [
          ...(live.catalog ?? []),
          {
            runbook_id: RB,
            slug: body.slug ?? '',
            title: 'Cache warmup',
            origin: 'ui',
            archived: false,
            latest_version: null,
            tags: [],
            draft_ahead: true,
          },
        ];
        replays.set(key, { body: JSON.stringify(body), source: body.source });
        return answer(body.source);
      }),
      http.patch('*/v1/runbooks/:rb/draft', async ({ request }) => {
        patchCount += 1;
        const body = (await request.json()) as { source: string };
        const ifMatch = request.headers.get('if-match');
        patches.push({ ifMatch, source: body.source });
        if (ifMatch !== etagOf(live.draft.revision)) return problem(412, 'catalog.draft_conflict');
        live.draft = { source: body.source, revision: live.draft.revision + 1 };
        if (options.losePatch?.(patchCount)) return HttpResponse.error();
        return HttpResponse.json(
          { runbook_id: RB, source: body.source, revision: live.draft.revision, diagnostics: [] },
          { headers: { ETag: etagOf(live.draft.revision) } },
        );
      }),
    ];
    return { posts, patches, handlers };
  }

  function renderNew(handlers: Parameters<typeof server.use>) {
    server.use(...handlers);
    tokenStore.set('trb_pat_test');
    const router = createAppRouter(createMemoryHistory({ initialEntries: ['/p/prod/runbooks'] }));
    render(<App router={router} />);
    return router;
  }

  const newLive = (): EditorServer => ({
    draft: { source: '', revision: 1 },
    latestVersion: null,
    scopes: EDITOR_SCOPES,
    catalog: [],
  });

  it('after an unanswered create and an edit, finds the runbook it made and saves the edit to it', async () => {
    const live = newLive();
    // The create lands; its answer is lost.
    const { posts, patches, handlers } = replayingServer(live, { losePost: (n) => n === 1 });
    const router = renderNew(handlers);
    const user = await openNew('Cache warmup');
    await user.click(screen.getByRole('button', { name: 'Save draft' }));
    expect(await screen.findByTestId('editor-notice')).toHaveTextContent(/did not (answer|reach)/);
    const sent = yamlBox().value;
    const edited = `${sent}# mine\n`;
    fireEvent.change(yamlBox(), { target: { value: edited } });
    await user.click(screen.getByRole('button', { name: 'Save draft' }));
    await waitFor(() =>
      expect(router.state.location.pathname).toBe(`/p/prod/runbooks/${RB}/editor`),
    );
    // UI-4b: the unanswered create is asked again with its own key and body
    // (the server replays what it did), then the edit is saved over it.
    expect(posts.map((p) => p.key)).toEqual([posts[0]?.key, posts[0]?.key]);
    expect(posts[1]?.body.source).toBe(sent);
    expect(patches).toEqual([{ ifMatch: etagOf(1), source: edited }]);
  });

  // UI-4b item 1.
  it('after an unanswered create and an unanswered save over it, the next save still reaches it', async () => {
    const live = newLive();
    // The create lands and its answer is lost; so does the first save over it.
    const { posts, patches, handlers } = replayingServer(live, {
      losePost: (n) => n === 1,
      losePatch: (n) => n === 1,
    });
    const router = renderNew(handlers);
    const user = await openNew('Cache warmup');
    await user.click(screen.getByRole('button', { name: 'Save draft' }));
    expect(await screen.findByTestId('editor-notice')).toHaveTextContent(/did not (answer|reach)/);
    const first = `${yamlBox().value}# one\n`;
    fireEvent.change(yamlBox(), { target: { value: first } });
    await user.click(screen.getByRole('button', { name: 'Save draft' }));
    await waitFor(() => expect(patches).toHaveLength(1));
    expect(await screen.findByTestId('editor-notice')).toHaveTextContent(/did not (answer|reach)/);
    const second = `${first}# two\n`;
    fireEvent.change(yamlBox(), { target: { value: second } });
    await user.click(screen.getByRole('button', { name: 'Save draft' }));
    await waitFor(() =>
      expect(router.state.location.pathname).toBe(`/p/prod/runbooks/${RB}/editor`),
    );
    expect(new Set(posts.map((p) => p.key)).size).toBe(1);
    expect(patches).toEqual([
      { ifMatch: etagOf(1), source: first },
      { ifMatch: etagOf(2), source: second },
    ]);
  });

  // UI-4b item 2.
  it('never saves over a runbook someone else made with the same slug and text', async () => {
    const live = newLive();
    // This person's create is lost on the way (the server never sees it).
    const { posts, patches, handlers } = replayingServer(live, { dropPost: (n) => n === 1 });
    renderNew(handlers);
    const user = await openNew('Cache warmup');
    await user.click(screen.getByRole('button', { name: 'Save draft' }));
    expect(await screen.findByTestId('editor-notice')).toHaveTextContent(/did not (answer|reach)/);
    // Meanwhile someone else creates the same slug from the same template.
    live.draft = { source: yamlBox().value, revision: 1 };
    live.catalog = [
      {
        runbook_id: RB,
        slug: 'cache-warmup',
        title: 'Cache warmup',
        origin: 'ui',
        archived: false,
        latest_version: null,
        tags: [],
        draft_ahead: true,
      },
    ];
    fireEvent.change(yamlBox(), { target: { value: `${yamlBox().value}# mine\n` } });
    await user.click(screen.getByRole('button', { name: 'Save draft' }));
    expect(await screen.findByTestId('editor-notice')).toHaveTextContent(
      'A runbook with that slug already exists in this project.',
    );
    expect(patches).toEqual([]);
    expect(posts.map((p) => p.body.source.endsWith('# mine\n'))).toEqual([false, true]);
  });

  it('after an unanswered create that did not land, an edited save is a new action', async () => {
    const posts: Post[] = [];
    server.use(createHandler(posts, [problem(503, 'storage.unavailable')]));
    renderRunbooks(EDITOR_SCOPES);
    const user = await openNew('Cache warmup');
    await user.click(screen.getByRole('button', { name: 'Save draft' }));
    expect(await screen.findByTestId('editor-notice')).toHaveTextContent('storage.unavailable');
    fireEvent.change(yamlBox(), { target: { value: `${yamlBox().value}# mine\n` } });
    await user.click(screen.getByRole('button', { name: 'Save draft' }));
    expect(await screen.findByRole('heading', { level: 1, name: 'Editor' })).toBeVisible();
    expect(posts).toHaveLength(2);
    expect(posts[1]?.key).not.toBe(posts[0]?.key);
    expect(posts[1]?.body.source).toContain('# mine');
  });

  it('checks the ID itself, not only the catalogue pages loaded, and refuses “new”', async () => {
    const row = (slug: string) => ({
      runbook_id: `rb_${slug}`,
      slug,
      title: slug,
      origin: 'ui' as const,
      archived: false,
      latest_version: 1,
      tags: [],
      draft_ahead: false,
    });
    renderRunbooks(EDITOR_SCOPES);
    // UI-1c: a large project whose list (searched or not) never reaches the
    // slug within any number of pages; only the runbook's own route has it.
    let searchedForIt = 0;
    server.use(
      http.get('*/v1/projects/:p/runbooks', ({ request }) => {
        if (new URL(request.url).searchParams.get('q')?.includes('db-failover')) searchedForIt += 1;
        return HttpResponse.json({
          items: Array.from({ length: 25 }, (_, i) => row(`other-${i}`)),
          next_cursor: 'page2',
        });
      }),
      http.get('*/v1/projects/:p/runbooks/:slug', ({ params }) =>
        params.slug === 'db-failover'
          ? HttpResponse.json(row('db-failover'))
          : problem(404, 'resource.not_found'),
      ),
    );
    const { user, dialog } = await openDialog();
    await user.type(within(dialog).getByLabelText('Name'), 'DB failover');
    await user.click(within(dialog).getByRole('button', { name: 'Continue to the builder' }));
    await waitFor(() =>
      expect(within(dialog).getByRole('alert')).toHaveTextContent(
        'A runbook with this ID already exists in this project.',
      ),
    );
    const id = within(dialog).getByLabelText('ID');
    await user.clear(id);
    await user.type(id, 'new');
    await user.click(within(dialog).getByRole('button', { name: 'Continue to the builder' }));
    await waitFor(() =>
      expect(within(dialog).getByRole('alert')).toHaveTextContent(/“new” is reserved/),
    );
    expect(screen.getByRole('dialog', { name: 'New runbook' })).toBeVisible();
    // The list was never walked for the slug.
    expect(searchedForIt).toBe(0);
  });
});
