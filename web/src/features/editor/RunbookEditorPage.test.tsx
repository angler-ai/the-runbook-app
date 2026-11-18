// The editor's server interplay (M5), over MSW: provisional browser
// diagnostics replaced by the server's (F99), the 412 "changed elsewhere"
// flow with both choices, idempotent publish retry, "unchanged" publish (D21),
// publication refusals, and the read-only state.
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { tokenStore } from '../../api/token-store';
import { rememberEditorTab } from '../runbook/tabs';
import { expectNoAxeViolations } from '../../test/axe';
import { server } from '../../test/msw/server';
import {
  RB,
  editorHandlers,
  etagOf,
  fakeCompiler,
  problem,
  renderEditor,
  type EditorServer,
} from './test-support';

vi.mock('./YamlEditor', async () => ({
  default: (await import('./test-support')).StubYamlEditor,
}));

const SOURCE = 'apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata:\n  id: post-deploy\n';
const EDITED = `${SOURCE}  title: Edited\n`;

const unknownTeam = {
  path: '/spec/steps/0/approval/approvers/teams/0',
  code: 'identity.unknown_team',
  message: 'unknown team oncall',
  line: 3,
  column: 5,
};

function state(overrides: Partial<EditorServer> = {}): EditorServer {
  return { draft: { source: SOURCE, revision: 2 }, latestVersion: 1, ...overrides };
}

const yaml = () => screen.findByRole('textbox', { name: /Runbook YAML/ });
const edit = async (text: string) => fireEvent.change(await yaml(), { target: { value: text } });
const diagnostics = () => screen.getByTestId('diagnostics');
const markers = () => screen.getByRole('list', { name: 'Editor markers' });

beforeEach(() => {
  tokenStore.set('trb_pat_test');
  // Every test starts on the default tab (Graph); the choice is remembered.
  rememberEditorTab('graph');
});
afterEach(() => tokenStore.clear());

describe('runbook editor', () => {
  it.each([
    [5, true],
    [7, false],
  ])('warns when the worst case is over the server’s step cap (%i)', async (cap, warns) => {
    const meta = (step_runs: number) =>
      http.get('*/v1/meta', () =>
        HttpResponse.json({
          edition: 'self_hosted',
          version: '0.0.0',
          schema: {},
          capabilities: {},
          limits: { max_request_bytes: 1, step_runs },
        }),
      );
    server.use(meta(cap), ...editorHandlers(state()));
    renderEditor(fakeCompiler(() => []));
    await waitFor(() =>
      expect(screen.getByTestId('step-budget')).toHaveTextContent('at most 7 step runs'),
    );
    if (warns) {
      expect(await screen.findByTestId('step-cap-warning')).toHaveTextContent(
        'Over this server’s limit of 5 steps per run: a run that reaches it stops.',
      );
    } else {
      await waitFor(() => expect(screen.queryByTestId('step-cap-warning')).toBeNull());
    }
  });

  it('shows provisional browser diagnostics until a save returns the server’s, which win', async () => {
    const patches: { ifMatch: string | null; source: string }[] = [];
    server.use(
      ...editorHandlers(state()),
      http.patch('*/v1/runbooks/:rb/draft', async ({ request }) => {
        const body = (await request.json()) as { source: string };
        patches.push({ ifMatch: request.headers.get('if-match'), source: body.source });
        return HttpResponse.json(
          { runbook_id: RB, source: body.source, revision: 3, diagnostics: [] },
          { headers: { ETag: etagOf(3) } },
        );
      }),
    );
    // A stale directory (F99): the browser reports a team the server knows.
    const { container } = renderEditor(fakeCompiler(() => [unknownTeam]));
    await yaml();

    expect(
      await within(markers()).findByText('provisional 3:5 identity.unknown_team'),
    ).toBeVisible();
    expect(within(diagnostics()).getByRole('heading')).toHaveTextContent(
      'Provisional diagnostics (1)',
    );
    expect(diagnostics()).toHaveTextContent('unknown team oncall');
    expect(screen.getByTestId('step-budget')).toHaveTextContent(
      'at most 7 step runs (provisional)',
    );

    await edit(EDITED);
    await userEvent.click(screen.getByRole('button', { name: 'Save draft' }));

    expect(
      await screen.findByText(/Saved revision 3\. The server found no problems/),
    ).toBeVisible();
    expect(patches).toEqual([{ ifMatch: etagOf(2), source: EDITED }]);
    expect(within(diagnostics()).getByRole('heading')).toHaveTextContent('Server diagnostics (0)');
    expect(within(markers()).queryAllByRole('listitem')).toHaveLength(0);
    await expectNoAxeViolations(container);

    // A new edit makes the server's verdict stale: provisional again.
    await edit(`${EDITED}# more\n`);
    expect(
      await within(markers()).findByText('provisional 3:5 identity.unknown_team'),
    ).toBeVisible();
    expect(within(diagnostics()).getByRole('heading')).toHaveTextContent(
      'Provisional diagnostics (1)',
    );
  });

  it('shows a diagnostic’s human message, with its code as secondary text (#11)', async () => {
    server.use(...editorHandlers(state()));
    renderEditor(
      fakeCompiler(() => [
        {
          path: '/spec/execution/timeout',
          code: 'spec.invalid_value',
          message: 'spec.invalid_value',
          line: 12,
          column: 14,
        },
      ]),
    );
    await yaml();
    const item = await within(diagnostics()).findByRole('listitem');
    expect(item).toHaveTextContent('“execution.timeout” has a value that is not allowed here.');
    // The code shows once, as secondary text.
    expect(within(item).getAllByText('spec.invalid_value')).toHaveLength(1);
    expect(item.textContent?.match(/spec\.invalid_value/g)).toHaveLength(1);
  });

  it('places server diagnostics without a span from their pointer', async () => {
    server.use(
      ...editorHandlers(state()),
      http.patch('*/v1/runbooks/:rb/draft', async ({ request }) => {
        const body = (await request.json()) as { source: string };
        return HttpResponse.json(
          {
            runbook_id: RB,
            source: body.source,
            revision: 3,
            diagnostics: [{ path: '/metadata/id', code: 'runbook.id_mismatch', message: 'id' }],
          },
          { headers: { ETag: etagOf(3) } },
        );
      }),
    );
    renderEditor(fakeCompiler(() => []));
    await edit(EDITED);
    await userEvent.click(screen.getByRole('button', { name: 'Save draft' }));
    expect(await within(markers()).findByText('server 4:7 runbook.id_mismatch')).toBeVisible();
    expect(screen.getByText(/The server found 1 problem;/)).toBeVisible();
  });

  it('on 412 reloads their newer draft without writing anything', async () => {
    const live = state();
    const patches: (string | null)[] = [];
    server.use(
      ...editorHandlers(live),
      http.patch('*/v1/runbooks/:rb/draft', ({ request }) => {
        patches.push(request.headers.get('if-match'));
        return problem(412, 'catalog.draft_conflict');
      }),
    );
    renderEditor(fakeCompiler(() => []));
    await edit(EDITED);
    // Someone else (the CLI) saves revision 3 meanwhile.
    live.draft = { source: `${SOURCE}  title: Theirs\n`, revision: 3 };
    await userEvent.click(screen.getByRole('button', { name: 'Save draft' }));

    const conflict = await screen.findByTestId('conflict');
    expect(conflict).toHaveTextContent('This draft changed elsewhere.');
    expect(conflict).toHaveTextContent('nothing was overwritten');
    await userEvent.click(within(conflict).getByRole('button', { name: 'Reload their version' }));

    await waitFor(async () => expect(await yaml()).toHaveValue(`${SOURCE}  title: Theirs\n`));
    expect(screen.queryByTestId('conflict')).not.toBeInTheDocument();
    expect(screen.getByText(/Loaded revision 3 from the server/)).toBeVisible();
    expect(screen.getByText('Saved, revision 3.')).toBeVisible();
    expect(patches).toEqual([etagOf(2)]);
  });

  it('reapply after a save whose answer was lost does not apply that save twice', async () => {
    const live = state();
    const FIRST = `${SOURCE}  title: First\n`;
    const SECOND = `${FIRST}  description: More\n`;
    let calls = 0;
    server.use(
      ...editorHandlers(live),
      http.patch('*/v1/runbooks/:rb/draft', async ({ request }) => {
        const body = (await request.json()) as { source: string };
        calls += 1;
        if (request.headers.get('if-match') !== etagOf(live.draft.revision))
          return problem(412, 'catalog.draft_conflict');
        live.draft = { source: body.source, revision: live.draft.revision + 1 };
        // The first save lands, but its answer never arrives.
        if (calls === 1) return HttpResponse.error();
        return HttpResponse.json(
          { runbook_id: RB, source: body.source, revision: live.draft.revision, diagnostics: [] },
          { headers: { ETag: etagOf(live.draft.revision) } },
        );
      }),
    );
    renderEditor(fakeCompiler(() => []));
    await edit(FIRST);
    await userEvent.click(screen.getByRole('button', { name: 'Save draft' }));
    expect(await screen.findByText(/Save did not reach the server/)).toBeVisible();
    await edit(SECOND);
    await userEvent.click(screen.getByRole('button', { name: 'Save draft' }));
    const conflict = await screen.findByTestId('conflict');
    await userEvent.click(
      within(conflict).getByRole('button', { name: 'Reload and reapply my changes' }),
    );
    expect(await screen.findByText(/reapplied your changes on top/)).toBeVisible();
    expect(await yaml()).toHaveValue(SECOND);
  });

  it('refuses to reapply when a save whose answer was lost may sit under a newer revision', async () => {
    const live = state();
    const FIRST = `${SOURCE}  title: First\n`;
    let calls = 0;
    server.use(
      ...editorHandlers(live),
      http.patch('*/v1/runbooks/:rb/draft', async ({ request }) => {
        const body = (await request.json()) as { source: string };
        calls += 1;
        if (request.headers.get('if-match') !== etagOf(live.draft.revision))
          return problem(412, 'catalog.draft_conflict');
        live.draft = { source: body.source, revision: live.draft.revision + 1 };
        return HttpResponse.error();
      }),
    );
    renderEditor(fakeCompiler(() => []));
    await edit(FIRST);
    await userEvent.click(screen.getByRole('button', { name: 'Save draft' }));
    expect(await screen.findByText(/Save did not reach the server/)).toBeVisible();
    // Someone else saves on top of it.
    live.draft = { source: `${FIRST}  tags: [theirs]\n`, revision: 4 };
    await edit(`${FIRST}  description: Mine\n`);
    await userEvent.click(screen.getByRole('button', { name: 'Save draft' }));
    const conflict = await screen.findByTestId('conflict');
    await userEvent.click(
      within(conflict).getByRole('button', { name: 'Reload and reapply my changes' }),
    );
    expect(await screen.findByText(/earlier save of yours/)).toBeVisible();
    expect(await yaml()).toHaveValue(`${FIRST}  description: Mine\n`);
    expect(calls).toBe(2);
  });

  it('on 412 keeps mine by saving it over the newer revision it just read', async () => {
    const live = state();
    const patches: { ifMatch: string | null; source: string }[] = [];
    server.use(
      ...editorHandlers(live),
      http.patch('*/v1/runbooks/:rb/draft', async ({ request }) => {
        const body = (await request.json()) as { source: string };
        const ifMatch = request.headers.get('if-match');
        patches.push({ ifMatch, source: body.source });
        if (ifMatch !== etagOf(live.draft.revision)) return problem(412, 'catalog.draft_conflict');
        live.draft = { source: body.source, revision: live.draft.revision + 1 };
        return HttpResponse.json(
          { runbook_id: RB, source: body.source, revision: live.draft.revision, diagnostics: [] },
          { headers: { ETag: etagOf(live.draft.revision) } },
        );
      }),
    );
    renderEditor(fakeCompiler(() => []));
    await edit(EDITED);
    live.draft = { source: `${SOURCE}  title: Theirs\n`, revision: 3 };
    await userEvent.click(screen.getByRole('button', { name: 'Save draft' }));
    const conflict = await screen.findByTestId('conflict');
    await userEvent.click(within(conflict).getByRole('button', { name: 'Keep mine and retry' }));

    expect(await screen.findByText(/Saved revision 4\./)).toBeVisible();
    expect(patches).toEqual([
      { ifMatch: etagOf(2), source: EDITED },
      { ifMatch: etagOf(3), source: EDITED },
    ]);
    expect(await yaml()).toHaveValue(EDITED);
    expect(screen.queryByTestId('conflict')).not.toBeInTheDocument();
  });

  // UI-4b item 5.
  it('keep mine over a draft that is exactly mine forgets the saves that got no answer', async () => {
    const live = state();
    const A = `${SOURCE}  title: A\n`;
    const B = `${SOURCE}  title: B\n`;
    const C = `${B}  description: Mine\n`;
    let calls = 0;
    server.use(
      ...editorHandlers(live),
      http.patch('*/v1/runbooks/:rb/draft', async ({ request }) => {
        const body = (await request.json()) as { source: string };
        calls += 1;
        if (request.headers.get('if-match') !== etagOf(live.draft.revision))
          return problem(412, 'catalog.draft_conflict');
        live.draft = { source: body.source, revision: live.draft.revision + 1 };
        // The first save lands as revision 3; its answer is lost.
        if (calls === 1) return HttpResponse.error();
        return HttpResponse.json(
          { runbook_id: RB, source: body.source, revision: live.draft.revision, diagnostics: [] },
          { headers: { ETag: etagOf(live.draft.revision) } },
        );
      }),
    );
    renderEditor(fakeCompiler(() => []));
    await edit(A);
    await userEvent.click(screen.getByRole('button', { name: 'Save draft' }));
    expect(await screen.findByText(/Save did not reach the server/)).toBeVisible();
    await edit(B);
    // Another tab of theirs saved exactly B on top (revision 4).
    live.draft = { source: B, revision: 4 };
    await userEvent.click(screen.getByRole('button', { name: 'Save draft' }));
    let conflict = await screen.findByTestId('conflict');
    await userEvent.click(within(conflict).getByRole('button', { name: 'Keep mine and retry' }));
    await waitFor(() => expect(screen.queryByTestId('conflict')).not.toBeInTheDocument());
    // Someone else saves revision 5; this person's next change merges onto it.
    live.draft = { source: `# theirs\n${B}`, revision: 5 };
    await edit(C);
    await userEvent.click(screen.getByRole('button', { name: 'Save draft' }));
    conflict = await screen.findByTestId('conflict');
    await userEvent.click(
      within(conflict).getByRole('button', { name: 'Reload and reapply my changes' }),
    );
    expect(await screen.findByText(/reapplied your changes on top/)).toBeVisible();
    expect(await yaml()).toHaveValue(`# theirs\n${C}`);
  });

  // UI-4b item 6.
  it('a save that fails while offline is not remembered as possibly landed', async () => {
    const live = state();
    const FIRST = `${SOURCE}  title: First\n`;
    const patches: string[] = [];
    server.use(
      ...editorHandlers(live),
      http.patch('*/v1/runbooks/:rb/draft', async ({ request }) => {
        const body = (await request.json()) as { source: string };
        patches.push(body.source);
        if (request.headers.get('if-match') !== etagOf(live.draft.revision))
          return problem(412, 'catalog.draft_conflict');
        return HttpResponse.error();
      }),
    );
    renderEditor(fakeCompiler(() => []));
    await edit(FIRST);
    const online = vi.spyOn(navigator, 'onLine', 'get').mockReturnValue(false);
    try {
      await userEvent.click(screen.getByRole('button', { name: 'Save draft' }));
      expect(
        await screen.findByText(/You appear to be offline: the save did not reach the server/),
      ).toBeVisible();
    } finally {
      online.mockRestore();
    }
    // It was still tried (a localhost or LAN server may answer while offline).
    expect(patches).toEqual([FIRST]);
    // Back online: someone else saved meanwhile; reapply merges (nothing of
    // this person's can be under their revision).
    live.draft = { source: `# theirs\n${SOURCE}`, revision: 3 };
    await userEvent.click(screen.getByRole('button', { name: 'Save draft' }));
    const conflict = await screen.findByTestId('conflict');
    await userEvent.click(
      within(conflict).getByRole('button', { name: 'Reload and reapply my changes' }),
    );
    expect(await screen.findByText(/reapplied your changes on top/)).toBeVisible();
  });

  it('still saves while the browser says it is offline, when the server answers (UI-4b review)', async () => {
    const live = state();
    const FIRST = `${SOURCE}  title: First\n`;
    const patches: string[] = [];
    server.use(
      ...editorHandlers(live),
      http.patch('*/v1/runbooks/:rb/draft', async ({ request }) => {
        const body = (await request.json()) as { source: string };
        patches.push(body.source);
        const revision = live.draft.revision + 1;
        live.draft = { source: body.source, revision };
        return HttpResponse.json(
          { runbook_id: RB, source: body.source, revision, diagnostics: [] },
          { headers: { ETag: etagOf(revision) } },
        );
      }),
    );
    renderEditor(fakeCompiler(() => []));
    await edit(FIRST);
    const online = vi.spyOn(navigator, 'onLine', 'get').mockReturnValue(false);
    try {
      await userEvent.click(screen.getByRole('button', { name: 'Save draft' }));
      expect(await screen.findByText(/^Saved revision/)).toBeVisible();
    } finally {
      online.mockRestore();
    }
    expect(patches).toEqual([FIRST]);
  });

  it('says why reapply refuses after a save that was sent but got no answer', async () => {
    const live = state();
    const FIRST = `${SOURCE}  title: First\n`;
    server.use(
      ...editorHandlers(live),
      http.patch('*/v1/runbooks/:rb/draft', async ({ request }) => {
        const body = (await request.json()) as { source: string };
        if (request.headers.get('if-match') !== etagOf(live.draft.revision))
          return problem(412, 'catalog.draft_conflict');
        live.draft = { source: body.source, revision: live.draft.revision + 1 };
        return HttpResponse.error();
      }),
    );
    renderEditor(fakeCompiler(() => []));
    await edit(FIRST);
    await userEvent.click(screen.getByRole('button', { name: 'Save draft' }));
    expect(await screen.findByText(/Save did not reach the server/)).toBeVisible();
    live.draft = { source: `${FIRST}  tags: [theirs]\n`, revision: 4 };
    await edit(`${FIRST}  description: Mine\n`);
    await userEvent.click(screen.getByRole('button', { name: 'Save draft' }));
    const conflict = await screen.findByTestId('conflict');
    await userEvent.click(
      within(conflict).getByRole('button', { name: 'Reload and reapply my changes' }),
    );
    expect(
      await screen.findByText(/was sent but no answer came back, so the server may have saved it/),
    ).toBeVisible();
  });

  it('retries a failed publish with the same Idempotency-Key, then a new publish gets a new key', async () => {
    const live = state({ latestVersion: 1 });
    const posts: { key: string | null; ifMatch: string | null }[] = [];
    server.use(
      ...editorHandlers(live),
      http.post('*/v1/runbooks/:rb/versions', ({ request }) => {
        posts.push({
          key: request.headers.get('idempotency-key'),
          ifMatch: request.headers.get('if-match'),
        });
        if (posts.length === 1) return problem(503, 'storage.unavailable');
        const version = posts.length === 2 ? 2 : (live.latestVersion ?? 0);
        live.latestVersion = version;
        return HttpResponse.json(
          { runbook_id: RB, runbook_version_id: `rbv_${version}`, runbook_version: version },
          { status: 201 },
        );
      }),
    );
    renderEditor(fakeCompiler(() => []));
    const publish = await screen.findByRole('button', { name: 'Publish' });

    await userEvent.click(publish);
    expect(
      await screen.findByText(/Publishing did not finish \(storage\.unavailable\)/),
    ).toBeVisible();
    await userEvent.click(screen.getByRole('button', { name: 'Publish' }));
    expect(await screen.findByText('Published v2.')).toBeVisible();

    await userEvent.click(screen.getByRole('button', { name: 'Publish' }));
    expect(await screen.findByText(/Unchanged; same as v2\./)).toBeVisible();

    expect(posts).toHaveLength(3);
    expect(posts.every((p) => p.ifMatch === etagOf(2))).toBe(true);
    expect(posts[0]?.key).toBeTruthy();
    expect(posts[1]?.key).toBe(posts[0]?.key);
    expect(posts[2]?.key).not.toBe(posts[0]?.key);
  });

  it('says "unchanged" when publishing returns the latest version (D21)', async () => {
    server.use(
      ...editorHandlers(state({ latestVersion: 3 })),
      http.post('*/v1/runbooks/:rb/versions', () =>
        HttpResponse.json(
          { runbook_id: RB, runbook_version_id: 'rbv_3', runbook_version: 3 },
          { status: 201 },
        ),
      ),
    );
    renderEditor(fakeCompiler(() => []));
    await userEvent.click(await screen.findByRole('button', { name: 'Publish' }));
    expect(
      await screen.findByText('Unchanged; same as v3. Nothing new was published.'),
    ).toBeVisible();
  });

  it('shows a publication refusal’s diagnostics as the server’s', async () => {
    server.use(
      ...editorHandlers(state()),
      http.post('*/v1/runbooks/:rb/versions', () =>
        problem(422, 'identity.unknown_team', [unknownTeam]),
      ),
    );
    renderEditor(fakeCompiler(() => []));
    await userEvent.click(await screen.findByRole('button', { name: 'Publish' }));
    expect(await screen.findByText(/Not published: the server found 1 problem/)).toBeVisible();
    expect(within(diagnostics()).getByRole('heading')).toHaveTextContent('Server diagnostics (1)');
    expect(within(markers()).getByText('server 3:5 identity.unknown_team')).toBeVisible();
  });

  it('publish on 412 keeps mine: saves over the newer draft, then publishes that revision', async () => {
    const live = state();
    const calls: string[] = [];
    server.use(
      ...editorHandlers(live),
      http.patch('*/v1/runbooks/:rb/draft', async ({ request }) => {
        const body = (await request.json()) as { source: string };
        calls.push(`PATCH ${request.headers.get('if-match')}`);
        live.draft = { source: body.source, revision: live.draft.revision + 1 };
        return HttpResponse.json(
          { runbook_id: RB, source: body.source, revision: live.draft.revision, diagnostics: [] },
          { headers: { ETag: etagOf(live.draft.revision) } },
        );
      }),
      http.post('*/v1/runbooks/:rb/versions', ({ request }) => {
        const ifMatch = request.headers.get('if-match');
        calls.push(`POST ${ifMatch}`);
        if (ifMatch !== etagOf(live.draft.revision)) return problem(412, 'catalog.draft_conflict');
        return HttpResponse.json(
          { runbook_id: RB, runbook_version_id: 'rbv_2', runbook_version: 2 },
          { status: 201 },
        );
      }),
    );
    renderEditor(fakeCompiler(() => []));
    await screen.findByRole('button', { name: 'Publish' });
    live.draft = { source: `${SOURCE}  title: Theirs\n`, revision: 3 };
    await userEvent.click(screen.getByRole('button', { name: 'Publish' }));
    const conflict = await screen.findByTestId('conflict');
    await userEvent.click(within(conflict).getByRole('button', { name: 'Keep mine and retry' }));
    expect(await screen.findByText('Published v2.')).toBeVisible();
    expect(calls).toEqual([`POST ${etagOf(2)}`, `PATCH ${etagOf(3)}`, `POST ${etagOf(4)}`]);
  });

  it('is read-only without runbooks:write', async () => {
    server.use(...editorHandlers(state({ scopes: ['runbooks:read', 'executions:read'] })));
    const { container } = renderEditor(fakeCompiler(() => []));
    const textbox = await screen.findByRole('textbox', { name: 'Runbook YAML (read only)' });
    expect(textbox).toHaveAttribute('readonly');
    expect(screen.getByTestId('read-only')).toHaveTextContent('runbooks:write');
    expect(screen.queryByRole('button', { name: 'Save draft' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Publish' })).not.toBeInTheDocument();
    await expectNoAxeViolations(container);
  });

  it('tells a reader who cannot create runbooks so, instead of an empty builder', async () => {
    server.use(...editorHandlers(state({ scopes: ['runbooks:read', 'executions:read'] })));
    const { container } = renderEditor(
      fakeCompiler(() => []),
      { isNew: true },
    );
    expect(
      await screen.findByRole('heading', { name: "You can't create runbooks in this project" }),
    ).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Back to Runbooks' })).toHaveAttribute(
      'href',
      '/p/prod/runbooks',
    );
    expect(screen.queryByRole('button', { name: 'Save draft' })).not.toBeInTheDocument();
    await expectNoAxeViolations(container);
  });

  it('is read-only for a Git-managed runbook', async () => {
    server.use(...editorHandlers(state({ origin: 'git' })));
    renderEditor(fakeCompiler(() => []));
    expect(await screen.findByTestId('read-only')).toHaveTextContent('managed in Git');
    expect(screen.queryByRole('button', { name: 'Save draft' })).not.toBeInTheDocument();
  });

  it('stays read-only until it knows whether the runbook is managed in Git', async () => {
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    server.use(
      http.get('*/v1/runbooks/:rb', async () => {
        await gate;
        return HttpResponse.json({
          runbook_id: RB,
          slug: 'post-deploy',
          title: 'Post deploy',
          origin: 'git',
          archived: false,
          latest_version: 1,
          tags: [],
          draft_ahead: false,
        });
      }),
      ...editorHandlers(state()),
    );
    renderEditor(fakeCompiler(() => []));
    const textbox = await screen.findByRole('textbox', { name: 'Runbook YAML (read only)' });
    expect(textbox).toHaveAttribute('readonly');
    expect(screen.getByTestId('read-only')).toHaveTextContent('checking whether');
    expect(screen.queryByRole('button', { name: 'Save draft' })).not.toBeInTheDocument();
    release();
    expect(
      await screen.findByText(/managed in Git\. Change it through a pull request/),
    ).toBeVisible();
    expect(screen.getByRole('textbox', { name: 'Runbook YAML (read only)' })).toHaveAttribute(
      'readonly',
    );
  });

  it('says when the browser cannot read the skill library', async () => {
    server.use(...editorHandlers(state({ skills: 'forbidden' })));
    renderEditor(fakeCompiler(() => []));
    expect(
      await screen.findByText(/cannot read the skill library \(your token lacks skills:read\)/),
    ).toBeVisible();
  });

  it('opens on Graph, keeps YAML mounted, and remembers the tab the person chose', async () => {
    const user = userEvent.setup();
    server.use(...editorHandlers(state()));
    const first = renderEditor(fakeCompiler(() => []));
    expect(await screen.findByRole('tab', { name: 'Graph' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    // The YAML editor is mounted (hidden) so Monaco is ready when shown.
    expect(await yaml()).toBeInTheDocument();
    await user.click(screen.getByRole('tab', { name: 'YAML' }));
    expect(screen.getByRole('tab', { name: 'YAML' })).toHaveAttribute('aria-selected', 'true');
    first.unmount();

    renderEditor(fakeCompiler(() => []));
    expect(await screen.findByRole('tab', { name: 'YAML' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
  });

  // UI-4: the graph is the builder, drawn from the YAML (not the compiler's outline).
  const CALL_SOURCE = `${SOURCE}spec:\n  steps:\n    - id: verify\n      title: Post-deploy checks\n      call:\n        runbook: post-deploy-checks\n`;
  const callLink = (container: HTMLElement) =>
    waitFor(() => {
      const el = container.querySelector<HTMLElement>('[data-slot="call-link"]');
      if (!el) throw new Error('no call link');
      return el;
    });

  it('links a call step in the graph to the child runbook in this project', async () => {
    server.use(...editorHandlers(state({ draft: { source: CALL_SOURCE, revision: 2 } })));
    const { container } = renderEditor(fakeCompiler(() => []));
    expect(await callLink(container)).toHaveAttribute(
      'href',
      '/p/prod/runbooks/post-deploy-checks',
    );
  });

  it('follows a call link in the app, and asks first when there are unsaved edits', async () => {
    server.use(...editorHandlers(state({ draft: { source: CALL_SOURCE, revision: 2 } })));
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    try {
      const { container, router } = renderEditor(fakeCompiler(() => []));
      await callLink(container);
      await edit(CALL_SOURCE.replace('title: Post-deploy checks', 'title: Checks'));
      // Unsaved edits: the person is asked, and staying keeps the editor.
      fireEvent.click(await callLink(container));
      await waitFor(() => expect(confirm).toHaveBeenCalledTimes(1));
      expect(router.state.location.pathname).toBe(`/p/prod/runbooks/${RB}/edit`);
      // Leaving anyway goes there in the app, with no reload.
      confirm.mockReturnValue(true);
      expect(fireEvent.click(await callLink(container))).toBe(false);
      await waitFor(() =>
        expect(router.state.location.pathname).toBe('/p/prod/runbooks/post-deploy-checks'),
      );
    } finally {
      confirm.mockRestore();
    }
  });

  it('says calls are checked by the server, not the browser', async () => {
    server.use(...editorHandlers(state()));
    renderEditor(fakeCompiler(() => []));
    expect(
      await within(await screen.findByTestId('diagnostics')).findByText(
        /Calls to other runbooks are checked only when the server validates/,
      ),
    ).toBeInTheDocument();
  });
});
