import { createMemoryHistory } from '@tanstack/react-router';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it } from 'vitest';
import { App } from '../../App';
import { tokenStore } from '../../api/token-store';
import { fixtures as shared } from '../../test/msw/fixtures';
import type { Schemas } from '../../api/client';
import startInputsInvalid from '../../../../api/examples/start-inputs-invalid.json';
import { createAppRouter } from '../../routes/router';
import { resolveRunbookId } from './StartRunDialog';
import { server } from '../../test/msw/server';

/** The checked-in start 422 (api/examples), typed against the schema. */
const fixtures = {
  ...shared,
  startInputsInvalid: startInputsInvalid as Schemas['Problem'],
};

const form = {
  inputs: {
    type: 'object',
    properties: {
      service: { type: 'string' },
      count: { type: 'integer' },
    },
    required: ['service'],
    additionalProperties: false,
  },
  fields: {
    type: 'object',
    properties: {
      note: { type: 'string' },
    },
    additionalProperties: false,
  },
};

interface StartCall {
  key: string | null;
  body: { inputs?: Record<string, unknown>; fields?: Record<string, unknown>; title?: unknown };
}

const problemJson = (body: { status: number }) =>
  HttpResponse.json(body, {
    status: body.status,
    headers: { 'Content-Type': 'application/problem+json' },
  });

/** Answers the nth start with `answers[n]` (a Problem) or, past the list, 201. */
function handlers(starts: StartCall[], answers: Schemas['Problem'][] = []) {
  return [
    http.get('*/v1/me', () =>
      HttpResponse.json({
        user: { id: 'usr_1', name: 'Alice' },
        org: { id: 'org_1', title: 'Acme', role: 'owner' },
        teams: [],
        credential: {
          kind: 'personal',
          actor_kind: 'human',
          scopes: ['runbooks:read', 'executions:read', 'executions:start'],
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
    http.get('*/v1/projects/:p/runbooks', () =>
      HttpResponse.json({
        items: [
          {
            runbook_id: 'rb_1',
            slug: 'post-deploy',
            origin: 'ui',
            archived: false,
            title: 'Post-deploy',
            latest_version: 3,
          },
        ],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/projects/:p/executions', () =>
      HttpResponse.json({ items: [], next_cursor: null }),
    ),
    http.get('*/v1/runbooks/:rb/versions', () =>
      HttpResponse.json({
        items: [
          {
            runbook_id: 'rb_1',
            runbook_version_id: 'rbv_1',
            runbook_version: 3,
            branch: null,
            commit_sha: null,
          },
        ],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/runbooks/:rb/versions/:version/form', () => HttpResponse.json(form)),
    http.post('*/v1/projects/:p/executions', async ({ request }) => {
      const answer = answers[starts.length];
      starts.push({
        key: request.headers.get('idempotency-key'),
        body: (await request.json()) as StartCall['body'],
      });
      if (answer) return problemJson(answer);
      return HttpResponse.json(
        { ...fixtures.postDeployCompleted, id: 'exe_new', status: 'running' },
        { status: 201 },
      );
    }),
    http.get('*/v1/executions/:exe/report', () => HttpResponse.json(fixtures.executionDocument)),
    http.get(
      '*/v1/executions/:exe/events',
      () =>
        new HttpResponse(new ReadableStream<Uint8Array>({ start() {} }), {
          headers: { 'Content-Type': 'text/event-stream' },
        }),
    ),
  ];
}

afterEach(() => tokenStore.clear());

async function openDialog(starts: StartCall[], answers: Schemas['Problem'][] = []) {
  server.use(...handlers(starts, answers));
  tokenStore.set('trb_pat_good');
  const router = createAppRouter(createMemoryHistory({ initialEntries: ['/p/prod/runbooks'] }));
  render(<App router={router} />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole('button', { name: /start a run/i }));
  const dialog = await screen.findByRole('dialog', { name: /start a run/i });
  await within(dialog).findByRole('textbox', { name: /service/i });
  return { router, user, dialog };
}

describe('start dialog', () => {
  it('submits what the form shows, not what was last saved (U9)', async () => {
    const starts: StartCall[] = [];
    const { router, user, dialog } = await openDialog(starts);
    const service = within(dialog).getByRole('textbox', { name: /service/i });
    await user.type(service, 'api');
    await user.click(within(dialog).getByRole('button', { name: /save inputs/i }));
    expect(within(dialog).getByText('Inputs saved.')).toBeInTheDocument();
    // Editing after saving: the saved notice goes and Start sends the edit.
    await user.clear(service);
    await user.type(service, 'web');
    expect(within(dialog).queryByText('Inputs saved.')).not.toBeInTheDocument();
    await user.type(within(dialog).getByRole('textbox', { name: /count/i }), '2');
    await user.type(within(dialog).getByRole('textbox', { name: /note/i }), 'go');
    await user.click(within(dialog).getByRole('button', { name: /^start run$/i }));
    expect(
      await screen.findByRole('heading', { name: /post-deploy verification/i }),
    ).toBeInTheDocument();
    expect(router.state.location.pathname).toBe('/x/exe_new');
    expect(starts.map((s) => [s.body.inputs, s.body.fields])).toEqual([
      [{ service: 'web', count: 2 }, { note: 'go' }],
    ]);
  });

  it('validates the shown values on Start and sends nothing while they are invalid', async () => {
    const starts: StartCall[] = [];
    const { user, dialog } = await openDialog(starts);
    await user.type(within(dialog).getByRole('textbox', { name: /count/i }), '1.5');
    await user.click(within(dialog).getByRole('button', { name: /^start run$/i }));
    const service = within(dialog).getByRole('textbox', { name: /service/i });
    expect(service).toHaveAttribute('aria-invalid', 'true');
    expect(service).toHaveAccessibleDescription(/this field is required/i);
    expect(service).toHaveFocus();
    expect(within(dialog).getByRole('textbox', { name: /count/i })).toHaveAccessibleDescription(
      /must be an integer/i,
    );
    expect(starts).toEqual([]);
  });

  it('puts a server 422 on its fields and retries the same action with the same key (U10, U2)', async () => {
    const starts: StartCall[] = [];
    const { router, user, dialog } = await openDialog(starts, [fixtures.startInputsInvalid]);
    const service = within(dialog).getByRole('textbox', { name: /service/i });
    await user.type(service, 'bogus');
    await user.click(within(dialog).getByRole('button', { name: /^start run$/i }));
    // The checked-in example names inputs.service and fields.note.
    await waitFor(() => expect(service).toHaveAttribute('aria-invalid', 'true'));
    expect(service).toHaveAccessibleDescription(fixtures.startInputsInvalid.errors[0]!.message);
    expect(service).toHaveFocus();
    const note = within(dialog).getByRole('textbox', { name: /note/i });
    expect(note).toHaveAttribute('aria-invalid', 'true');
    expect(note).toHaveAccessibleDescription(fixtures.startInputsInvalid.errors[1]!.message);
    expect(within(dialog).getByRole('alert')).toHaveTextContent(/check the marked fields/i);

    await user.clear(service);
    await user.type(service, 'api');
    await user.type(note, 'go');
    await user.click(within(dialog).getByRole('button', { name: /^start run$/i }));
    expect(
      await screen.findByRole('heading', { name: /post-deploy verification/i }),
    ).toBeInTheDocument();
    expect(router.state.location.pathname).toBe('/x/exe_new');
    expect(starts).toHaveLength(2);
    expect(starts[0]?.key).toBeTruthy();
    expect(starts[1]?.key).toBe(starts[0]?.key);
    expect(starts[1]?.body.inputs).toEqual({ service: 'api' });
    expect(starts[1]?.body.fields).toEqual({ note: 'go' });
  });

  it('reports errors for values the form does not show in the alert', async () => {
    const starts: StartCall[] = [];
    const unshown: Schemas['Problem'] = {
      ...fixtures.startInputsInvalid,
      errors: [
        { path: 'inputs.region', code: 'data.required', message: 'This value is required.' },
      ],
    };
    const { user, dialog } = await openDialog(starts, [unshown]);
    await user.type(within(dialog).getByRole('textbox', { name: /service/i }), 'api');
    await user.click(within(dialog).getByRole('button', { name: /^start run$/i }));
    expect(await within(dialog).findByRole('alert')).toHaveTextContent(
      'inputs.region: This value is required.',
    );
  });

  it('says a reused key means this dialog already started a run (U21)', async () => {
    const starts: StartCall[] = [];
    const { user, dialog } = await openDialog(starts, [
      {
        ...fixtures.startInputsInvalid,
        type: 'urn:runbook:problem:request.idempotency_key_reused',
        title: 'request.idempotency_key_reused',
        status: 409,
        detail: 'request.idempotency_key_reused',
        code: 'request.idempotency_key_reused',
        errors: [],
      },
    ]);
    await user.type(within(dialog).getByRole('textbox', { name: /service/i }), 'api');
    await user.click(within(dialog).getByRole('button', { name: /^start run$/i }));
    const alert = await within(dialog).findByRole('alert');
    expect(alert).toHaveTextContent(/this dialog already started a run/i);
    expect(within(alert).getByRole('link', { name: /executions/i })).toHaveAttribute(
      'href',
      '/p/prod/executions',
    );
  });
});

// R54: the dialog opened before the runbook catalog loads must still offer
// the form once the catalog arrives, keyed on the resolved runbook/version.
describe('start dialog opened before the catalog loads (R54)', () => {
  function heldCatalog() {
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => (release = resolve));
    const formCalls: string[] = [];
    const startBodies: { runbook?: string; version?: number | string }[] = [];
    const catalog = {
      items: [
        {
          runbook_id: 'rb_1',
          slug: 'post-deploy',
          origin: 'ui',
          archived: false,
          title: 'Post-deploy',
          latest_version: 3,
        },
      ],
      next_cursor: null,
    };
    server.use(
      // MSW tries handlers in list order: these overrides come first.
      http.get('*/v1/projects/:p/runbooks', async () => {
        await held;
        return HttpResponse.json(catalog);
      }),
      http.get('*/v1/runbooks/:rb/versions/:version/form', ({ params }) => {
        formCalls.push(`${String(params.rb)}@${String(params.version)}`);
        return HttpResponse.json(form);
      }),
      http.post('*/v1/projects/:p/executions', async ({ request }) => {
        startBodies.push((await request.json()) as { runbook?: string; version?: number });
        return HttpResponse.json(
          { ...fixtures.postDeployCompleted, id: 'exe_new', status: 'running' },
          { status: 201 },
        );
      }),
      ...handlers([]),
    );
    tokenStore.set('trb_pat_good');
    const router = createAppRouter(createMemoryHistory({ initialEntries: ['/p/prod/runbooks'] }));
    render(<App router={router} />);
    return { release, formCalls, startBodies, router };
  }

  it('loads the latest version’s form once the catalog arrives', async () => {
    const sim = heldCatalog();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /start a run/i }));
    const dialog = await screen.findByRole('dialog', { name: /start a run/i });
    sim.release();
    expect(
      await within(dialog).findByRole('option', { name: 'Post-deploy' }, { timeout: 3000 }),
    ).toBeInTheDocument();
    expect(within(dialog).getByLabelText('Runbook')).toHaveValue('rb_1');
    expect(
      await within(dialog).findByRole('button', { name: /save inputs/i }, { timeout: 3000 }),
    ).toBeInTheDocument();
    expect(sim.formCalls).toEqual(['rb_1@latest']);
  });

  it('follows an explicit version and starts that version', async () => {
    const sim = heldCatalog();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /start a run/i }));
    const dialog = await screen.findByRole('dialog', { name: /start a run/i });
    sim.release();
    const versionSelect = within(dialog).getByLabelText('Version');
    await within(dialog).findByRole('option', { name: 'v3' }, { timeout: 3000 });
    await user.selectOptions(versionSelect, '3');
    await within(dialog).findByRole('button', { name: /save inputs/i });
    expect(sim.formCalls).toContain('rb_1@3');
    await user.type(within(dialog).getByLabelText(/service/i), 'api');
    await user.click(within(dialog).getByRole('button', { name: /save inputs/i }));
    await user.click(within(dialog).getByRole('button', { name: /save fields/i }));
    const startButton = within(dialog).getByRole('button', { name: /^start run$/i });
    expect(startButton).toBeEnabled();
    await user.click(startButton);
    expect(
      await screen.findByRole('heading', { name: /post-deploy verification/i }),
    ).toBeInTheDocument();
    expect(sim.startBodies).toEqual([
      expect.objectContaining({ runbook: 'post-deploy', version: 3 }),
    ]);
  });
});

describe('resolveRunbookId (R54)', () => {
  const rb = (runbook_id: string) =>
    ({ runbook_id, slug: runbook_id, origin: 'ui', archived: false }) as Parameters<
      typeof resolveRunbookId
    >[1][number];
  it('waits for the catalog, then follows the choice or falls back to the first', () => {
    expect(resolveRunbookId(undefined, [])).toBe('');
    expect(resolveRunbookId('rb_2', [])).toBe('rb_2');
    expect(resolveRunbookId(undefined, [rb('rb_1'), rb('rb_2')])).toBe('rb_1');
    expect(resolveRunbookId('rb_2', [rb('rb_1'), rb('rb_2')])).toBe('rb_2');
    expect(resolveRunbookId('rb_gone', [rb('rb_1')])).toBe('rb_1');
  });
});
