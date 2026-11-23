import { createMemoryHistory } from '@tanstack/react-router';
import { cleanup, configure, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { App } from '../../App';
import type { Schemas } from '../../api/client';
import { tokenStore } from '../../api/token-store';
import { createAppRouter } from '../../routes/router';
import { fixtures } from '../../test/msw/fixtures';
import { server } from '../../test/msw/server';
import { CANCELLED_RUN_STREAM } from './fixtures/server-events';
import { forgetLiveResumePoints } from './live';
import { tabClaims } from './session';

type Doc = Schemas['ExecutionDocumentView'];

// The whole app renders per test; under a parallel run the default 1 s
// wait for findBy*/waitFor is tight.
configure({ asyncUtilTimeout: 3000 });

const EXE = 'exe_01JABCDEFGHJKMNPQRSTVWXYZ0';

function runningDoc(mutate?: (doc: Doc) => void): Doc {
  const doc = structuredClone(fixtures.executionDocument) as Doc;
  doc.header.status = 'running';
  doc.header.phase = 'ordinary';
  doc.header.project_id = 'prj_1';
  doc.sections = doc.sections.slice(0, 2);
  doc.header.steps = doc.header.steps.slice(0, 2);
  const section = doc.sections[0];
  const step = doc.header.steps[0];
  if (section && step) {
    section.status = 'ready';
    section.title = 'Health endpoint returns 200';
    section.actor = null;
    step.status = 'ready';
    step.actor = null;
    step.claim = null;
    step.checklist = [{ title: 'Login page loads', completed: false }];
  }
  mutate?.(doc);
  return doc;
}

const meBody = {
  user: { id: 'usr_1', name: 'Alice' },
  org: { id: 'org_1', title: 'Acme', role: 'member' },
  teams: [{ id: 'team_1', slug: 'oncall' }],
  credential: {
    kind: 'personal',
    actor_kind: 'human',
    scopes: ['runbooks:read', 'executions:read', 'executions:act', 'executions:start'],
    projects: null,
    expires_at_ms: Date.now() + 3_600_000,
  },
};

/** A resume brief listing `ready` as ready work (human unless stated). */
function briefFor(ready: { sr: string; actor?: 'human' | 'agent' }[]): Schemas['ResumeBrief'] {
  return {
    schema_revision: 1,
    execution_id: EXE,
    url: `/v1/executions/${EXE}`,
    web_url: `/x/${EXE}`,
    completed_steps: [],
    decisions: [],
    comments: [],
    ready_work: ready.map(({ sr, actor }) => ({
      step_run_id: sr,
      path: sr,
      title: sr,
      actor_kind: actor ?? 'human',
    })),
    next_action: ready.length > 0 ? 'claim_ready_work' : 'wait',
  };
}

/**
 * The simulated server agrees with this tab about the claims it granted:
 * a ready step this tab claimed reads as claimed by Alice with that claim,
 * as the real document would after the claim POST. U5 tests turn this off
 * to serve a document that disagrees with the tab.
 */
function withTabClaims(doc: Doc): Doc {
  const out = structuredClone(doc);
  for (const section of out.sections) {
    const claim = tabClaims.get(section.step_run_id);
    const step = out.header.steps.find((s) => s.id === section.step_run_id);
    if (!claim || !step || !['ready', 'claimed'].includes(section.status)) continue;
    section.status = 'claimed';
    section.actor = { kind: 'human', user_id: 'usr_1' };
    step.status = 'claimed';
    step.actor = { kind: 'human', user_id: 'usr_1' };
    step.claim = claim;
  }
  return out;
}

interface HandlerOptions {
  reflectTabClaims?: boolean;
  /** Extra scopes on the signed-in token (for example executions:admin). */
  extraScopes?: string[];
  /** Alice's role in prj_1; null means not a member. */
  projectRole?: Schemas['ProjectRole'] | null;
}

function baseHandlers(doc: Doc | (() => Doc), options: HandlerOptions = {}) {
  const read = typeof doc === 'function' ? doc : () => doc;
  const current = () => (options.reflectTabClaims === false ? read() : withTabClaims(read()));
  const role = options.projectRole === undefined ? 'operator' : options.projectRole;
  const me = {
    ...meBody,
    credential: {
      ...meBody.credential,
      scopes: [...meBody.credential.scopes, ...(options.extraScopes ?? [])],
    },
  };
  return [
    http.get('*/v1/me', () => HttpResponse.json(me)),
    http.get('*/v1/projects/:p/members', () =>
      HttpResponse.json({
        items: [{ user_id: 'usr_0', role: 'admin' }, ...(role ? [{ user_id: 'usr_1', role }] : [])],
        next_cursor: null,
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
      HttpResponse.json({ items: [], next_cursor: null }),
    ),
    http.get('*/v1/projects/:p/executions', () =>
      HttpResponse.json({ items: [], next_cursor: null }),
    ),
    // The human-audience brief (R55): the first section is a person's work.
    http.get('*/v1/executions/:exe/next', () =>
      HttpResponse.json(briefFor([{ sr: current().sections[0]?.step_run_id ?? '' }])),
    ),
    http.get('*/v1/executions/:exe/report', ({ request }) => {
      const format = new URL(request.url).searchParams.get('format');
      if (format === 'markdown') {
        return HttpResponse.text('# Report\n\nhello', {
          headers: { 'Content-Type': 'text/markdown' },
        });
      }
      if (format === 'junit') {
        return HttpResponse.text('<testsuites></testsuites>', {
          headers: { 'Content-Type': 'application/xml' },
        });
      }
      if (format === 'pdf') {
        return new HttpResponse(new Blob(['%PDF-1.4']), {
          headers: { 'Content-Type': 'application/pdf' },
        });
      }
      return HttpResponse.json(current());
    }),
    http.get(
      '*/v1/executions/:exe/events',
      () =>
        new HttpResponse(new ReadableStream<Uint8Array>({ start() {} }), {
          headers: { 'Content-Type': 'text/event-stream' },
        }),
    ),
  ];
}

function renderDoc() {
  tokenStore.set('trb_pat_good');
  const router = createAppRouter(createMemoryHistory({ initialEntries: [`/x/${EXE}?view=steps`] }));
  return { router, ...render(<App router={router} />) };
}

/** UI-3: the Markdown download is in the header's Export menu. */
async function exportMarkdown(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole('button', { name: /^export$/i }));
  await user.click(await screen.findByRole('menuitem', { name: /markdown/i }));
}

/** Click an approval option and confirm the vote (every vote is confirmed, U16). */
async function vote(user: ReturnType<typeof userEvent.setup>, actions: HTMLElement, name: RegExp) {
  await user.click(within(actions).getByRole('button', { name }));
  const dialog = await screen.findByRole('alertdialog');
  await user.click(within(dialog).getByRole('button', { name }));
}

afterEach(() => {
  forgetLiveResumePoints();
  tokenStore.clear();
  for (const id of Object.keys(tabClaims.all())) tabClaims.clear(id);
});

it('builds the Cloud Trace link using the authenticated run report', async () => {
  const traceId = '1234567890abcdef1234567890abcdef';
  const doc = runningDoc((view) => {
    view.trace_project = 'runbook-fixture';
    view.header.steps[0]!.runner = {
      agent_id: 'agt_fixture',
      agent_name: 'Checker',
      trace_id: traceId,
    };
  });
  server.use(...baseHandlers(doc));
  renderDoc();
  expect(await screen.findByRole('link', { name: traceId })).toHaveAttribute(
    'href',
    `https://console.cloud.google.com/traces/list?tid=${traceId}&project=runbook-fixture`,
  );
});

describe('execution document actions', () => {
  it('claims before reading the step, then rolls back an optimistic checklist tick on 409', async () => {
    const order: string[] = [];
    server.use(
      ...baseHandlers(runningDoc()),
      http.post('*/v1/step-runs/:sr/claim', async ({ request }) => {
        order.push('claim');
        const body = (await request.json()) as { session_id?: string; actor?: { kind?: string } };
        expect(body.session_id).toBeTruthy();
        expect(body.actor).toEqual({ kind: 'human' });
        return HttpResponse.json({ id: 'clm_1', generation: 1 });
      }),
      http.get('*/v1/step-runs/:sr', () => {
        order.push('read');
        return HttpResponse.json({
          step_run_id: 'sr_x',
          instructions: 'Call /healthz and record the status.',
          checklist: ['Login page loads'],
          parameters: {},
          output_schema: { type: 'object', properties: {} },
          skill: null,
        });
      }),
      http.post('*/v1/step-runs/:sr/checklist', () => {
        order.push('checklist');
        return HttpResponse.json(
          {
            code: 'claim.conflict',
            status: 409,
            detail: 'x',
            type: 't',
            title: 't',
            instance: 'urn:request:req_1',
            errors: [],
          },
          { status: 409, headers: { 'Content-Type': 'application/problem+json' } },
        );
      }),
    );
    renderDoc();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /claim this step/i }));
    expect(await screen.findByText(/call \/healthz/i)).toBeInTheDocument();
    expect(order).toEqual(['claim', 'read']);
    const checkbox = screen.getByRole('checkbox', { name: /login page loads/i });
    await user.click(checkbox);
    expect(await screen.findByText(/just claimed by someone else/i)).toBeInTheDocument();
    // The claim is gone (U5): its form and local ticks go with it.
    await waitFor(() =>
      expect(screen.queryByRole('checkbox', { name: /login page loads/i })).not.toBeInTheDocument(),
    );
    expect(tabClaims.all()).toEqual({});
    expect(order).toContain('checklist');
  });

  it('shows claimed-by-you-elsewhere with a release action (F100)', async () => {
    let released = false;
    const doc = runningDoc((d) => {
      const section = d.sections[0];
      const step = d.header.steps[0];
      if (section && step) {
        section.status = 'claimed';
        section.actor = { kind: 'human', user_id: 'usr_1' };
        step.status = 'claimed';
        step.actor = { kind: 'human', user_id: 'usr_1' };
        step.claim = { id: 'clm_other', generation: 2 };
      }
    });
    server.use(
      ...baseHandlers(doc),
      http.post('*/v1/step-runs/:sr/release', () => {
        released = true;
        return HttpResponse.json({});
      }),
    );
    renderDoc();
    const user = userEvent.setup();
    expect(await screen.findByText(/claimed by you elsewhere/i)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: /release it here/i }));
    expect(released).toBe(true);
  });

  it('decides an eligible approval through the decisions route', async () => {
    let decided: { option?: string } | null = null;
    const doc = runningDoc((d) => {
      const section = d.sections[0];
      if (section) {
        section.status = 'waiting';
        section.approval = {
          schema_revision: 1,
          step_run_id: section.step_run_id,
          status: 'waiting',
          prompt: 'Checks failed. Choose an action.',
          options: [
            { id: 'keep', title: 'Keep deployment', auto: false },
            { id: 'rollback', title: 'Roll back', auto: false },
          ],
          eligible_recipients: [{ kind: 'team', reference: 'oncall' }],
          escalation_recipients: [],
          escalated: false,
          quorum: 1,
          decisions: [],
        };
      }
    });
    server.use(
      ...baseHandlers(doc),
      http.post('*/v1/step-runs/:sr/decisions', async ({ request }) => {
        decided = (await request.json()) as { option?: string };
        return HttpResponse.json({
          by: 'human',
          user_id: 'usr_1',
          membership_revision: 1,
          option: 'keep',
        });
      }),
    );
    renderDoc();
    const user = userEvent.setup();
    const actions = await screen.findByTestId(/actions-/);
    await vote(user, actions, /keep deployment/i);
    await waitFor(() => expect(decided).toMatchObject({ option: 'keep' }));
  });

  // R1-D2: a runner agent reviewing an approval shows on its card, and
  // people can still decide; its decision links the runner transcript.
  it('shows a runner agent reviewing an approval, then its decision with the transcript', async () => {
    let decided: { option?: string } | null = null;
    let reviewing = true;
    const doc = () =>
      runningDoc((d) => {
        const section = d.sections[0];
        const step = d.header.steps[0];
        if (!section || !step) return;
        section.kind = 'approval';
        step.kind = 'approval';
        section.status = reviewing ? 'waiting' : 'succeeded';
        step.status = section.status;
        if (!reviewing) {
          section.outputs = { decision: 'ship', comment: '', decided_by: 'runner:rin_1' };
        }
        step.runner = {
          agent_id: 'agt_1',
          agent_name: 'release-reviewer',
          decision_ready_at_ms: 1,
          ...(reviewing && {
            decision_lease: { lease_id: 'clm_1', agent_name: 'release-reviewer' },
          }),
        };
        section.approval = {
          schema_revision: 1,
          step_run_id: section.step_run_id,
          status: section.status,
          prompt: 'Ship 2.4.0?',
          options: [
            { id: 'ship', title: 'Ship it', auto: false },
            { id: 'hold', title: 'Hold the release', auto: false },
          ],
          eligible_recipients: [{ kind: 'team', reference: 'oncall' }],
          escalation_recipients: [],
          escalated: false,
          quorum: 1,
          decisions: reviewing
            ? []
            : [
                {
                  by: 'agent',
                  principal: 'runner:rin_1',
                  agent_name: 'release-reviewer',
                  model: 'claude-sonnet',
                  session_id: 'clm_1',
                  option: 'ship',
                  comment: 'Canary is healthy [call 1].',
                  evidence: ['[call 1]'],
                },
              ],
          ...(reviewing && {
            decision_lease: { lease_id: 'clm_1', agent_name: 'release-reviewer' },
          }),
        };
      });
    server.use(
      ...baseHandlers(doc),
      http.post('*/v1/step-runs/:sr/decisions', async ({ request }) => {
        decided = (await request.json()) as { option?: string };
        return HttpResponse.json({
          by: 'human',
          user_id: 'usr_1',
          membership_revision: 1,
          option: 'hold',
        });
      }),
    );
    const view = renderDoc();
    const user = userEvent.setup();
    const actions = await screen.findByTestId(/actions-/);
    expect(
      await within(actions).findByText(/You can still decide; the first decision wins\./),
    ).toBeInTheDocument();
    expect(within(actions).getByRole('status')).toHaveTextContent(
      'Agent release-reviewer is reviewing.',
    );
    await vote(user, actions, /hold the release/i);
    await waitFor(() => expect(decided).toMatchObject({ option: 'hold' }));
    view.unmount();

    reviewing = false;
    renderDoc();
    const decision = await screen.findByText(/Decided by agent/);
    const card = decision.closest('[data-slot="agent-decision"]') as HTMLElement;
    // The headline names the agent, not its runner principal.
    expect(screen.getByText('Decided: Ship it by agent release-reviewer')).toBeInTheDocument();
    expect(card).toHaveTextContent('Decided by agent release-reviewer (claude-sonnet)');
    expect(card).toHaveTextContent('Runner-reported evidence: [call 1]');
    expect(within(card).getByRole('link', { name: 'Runner transcript' })).toHaveAttribute(
      'href',
      expect.stringMatching(/^\/p\/prj_1\/agents\/transcripts\/sr_/),
    );
  });

  it('keeps a waiting run live with actions available (R50)', async () => {
    const doc = runningDoc((d) => {
      d.header.status = 'waiting';
    });
    server.use(...baseHandlers(doc));
    renderDoc();
    // jsdom may not deliver the open stream head, so Connecting/Polling
    // also prove liveness; Completed/expired would mean the loop stopped.
    expect(await screen.findByText(/^(Live|Polling for updates|Connecting…)$/)).toBeInTheDocument();
    expect(await screen.findByRole('button', { name: /claim this step/i })).toBeInTheDocument();
  });

  it('shows a 409 approval.deadline_exceeded as a closed approval', async () => {
    const doc = runningDoc((d) => {
      const section = d.sections[0];
      if (section) {
        section.status = 'waiting';
        section.approval = {
          schema_revision: 1,
          step_run_id: section.step_run_id,
          status: 'waiting',
          prompt: 'Checks failed. Choose an action.',
          options: [{ id: 'keep', title: 'Keep deployment', auto: false }],
          eligible_recipients: [{ kind: 'team', reference: 'oncall' }],
          escalation_recipients: [],
          escalated: false,
          quorum: 1,
          decisions: [],
        };
      }
    });
    server.use(
      ...baseHandlers(doc),
      http.post('*/v1/step-runs/:sr/decisions', () =>
        HttpResponse.json(
          {
            code: 'approval.deadline_exceeded',
            status: 409,
            detail: 'deadline exceeded',
            type: 't',
            title: 't',
            instance: 'urn:request:req_1',
            errors: [],
          },
          { status: 409, headers: { 'Content-Type': 'application/problem+json' } },
        ),
      ),
    );
    renderDoc();
    const user = userEvent.setup();
    const actions = await screen.findByTestId(/actions-/);
    await vote(user, actions, /keep deployment/i);
    expect(await screen.findByText('This approval has closed.')).toBeInTheDocument();
  });

  it('loads logs lazily, pages forward, and reports 410 as expired', async () => {
    let logCalls = 0;
    server.use(
      ...baseHandlers(runningDoc()),
      http.get('*/v1/step-runs/:sr/logs', ({ request }) => {
        logCalls += 1;
        const cursor = new URL(request.url).searchParams.get('cursor') ?? '0';
        if (cursor === '0') {
          return HttpResponse.json({
            lines: [{ seq: 1, line: 'first' }],
            next_cursor: '1',
            has_more: true,
          });
        }
        return HttpResponse.json({
          lines: [{ seq: 2, line: 'second' }],
          next_cursor: '2',
          has_more: false,
        });
      }),
    );
    renderDoc();
    const user = userEvent.setup();
    expect(logCalls).toBe(0);
    const showers = await screen.findAllByRole('button', { name: /show log/i });
    await user.click(showers[0] as HTMLElement);
    expect(await screen.findByText('first')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: /load more logs/i }));
    expect(await screen.findByText('second')).toBeInTheDocument();
    expect(logCalls).toBe(2);
  });

  it('shows history expired for a 410 log page', async () => {
    server.use(
      ...baseHandlers(runningDoc()),
      http.get('*/v1/step-runs/:sr/logs', () =>
        HttpResponse.json(
          {
            code: 'execution.history_expired',
            status: 410,
            detail: 'x',
            type: 't',
            title: 't',
            instance: 'urn:request:req_1',
            errors: [],
          },
          { status: 410, headers: { 'Content-Type': 'application/problem+json' } },
        ),
      ),
    );
    renderDoc();
    const user = userEvent.setup();
    const showers = await screen.findAllByRole('button', { name: /show log/i });
    await user.click(showers[0] as HTMLElement);
    expect(
      await screen.findByText(/log history has expired/i, {}, { timeout: 4000 }),
    ).toBeInTheDocument();
  });

  it('downloads the markdown rendering', async () => {
    server.use(...baseHandlers(runningDoc()));
    const createObjectURL = vi.fn(() => 'blob:report');
    URL.createObjectURL = createObjectURL;
    URL.revokeObjectURL = vi.fn();
    renderDoc();
    const user = userEvent.setup();
    await exportMarkdown(user);
    await waitFor(() => expect(createObjectURL).toHaveBeenCalled());
  });

  it('shows four export format options in order', async () => {
    server.use(...baseHandlers(runningDoc()));
    renderDoc();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /^export$/i }));
    const items = await screen.findAllByRole('menuitem');
    expect(items.map((item) => item.textContent?.trim())).toEqual([
      'Markdown (.md)',
      'JUnit XML (.xml)',
      'PDF (.pdf)',
      'JSON (.json)',
    ]);
  });

  it('choosing JUnit requests format=junit and saves the XML file', async () => {
    let requestedFormat: string | null = null;
    server.use(
      http.get('*/v1/executions/:exe/report', ({ request }) => {
        const format = new URL(request.url).searchParams.get('format');
        if (format === 'junit') {
          requestedFormat = format;
          return HttpResponse.text('<testsuites></testsuites>', {
            headers: { 'Content-Type': 'application/xml' },
          });
        }
        return HttpResponse.json(runningDoc());
      }),
      ...baseHandlers(runningDoc()),
    );
    const createObjectURL = vi.fn(() => 'blob:junit');
    URL.createObjectURL = createObjectURL;
    URL.revokeObjectURL = vi.fn();
    let downloadedName: string | null = null;
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (
      this: HTMLAnchorElement,
    ) {
      downloadedName = this.download;
    });
    renderDoc();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /^export$/i }));
    await user.click(await screen.findByRole('menuitem', { name: /junit xml/i }));
    await waitFor(() => expect(requestedFormat).toBe('junit'));
    await waitFor(() => expect(downloadedName).toBe('post-deploy-verification.xml'));
    click.mockRestore();
  });

  it('choosing PDF requests format=pdf and saves the PDF file', async () => {
    let requestedFormat: string | null = null;
    server.use(
      http.get('*/v1/executions/:exe/report', ({ request }) => {
        const format = new URL(request.url).searchParams.get('format');
        if (format === 'pdf') {
          requestedFormat = format;
          return new HttpResponse(new Blob(['%PDF-1.4']), {
            headers: { 'Content-Type': 'application/pdf' },
          });
        }
        return HttpResponse.json(runningDoc());
      }),
      ...baseHandlers(runningDoc()),
    );
    const createObjectURL = vi.fn(() => 'blob:pdf');
    URL.createObjectURL = createObjectURL;
    URL.revokeObjectURL = vi.fn();
    let downloadedName: string | null = null;
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (
      this: HTMLAnchorElement,
    ) {
      downloadedName = this.download;
    });
    renderDoc();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /^export$/i }));
    await user.click(await screen.findByRole('menuitem', { name: /pdf/i }));
    await waitFor(() => expect(requestedFormat).toBe('pdf'));
    await waitFor(() => expect(downloadedName).toBe('post-deploy-verification.pdf'));
    click.mockRestore();
  });

  it('displays the control error when a PDF export fails', async () => {
    server.use(
      http.get('*/v1/executions/:exe/report', ({ request }) => {
        if (new URL(request.url).searchParams.get('format') === 'pdf') {
          return HttpResponse.json(
            { code: 'server.error', detail: 'PDF generation failed' },
            { status: 500, headers: { 'Content-Type': 'application/problem+json' } },
          );
        }
        return HttpResponse.json(runningDoc());
      }),
      ...baseHandlers(runningDoc()),
    );
    renderDoc();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /^export$/i }));
    await user.click(await screen.findByRole('menuitem', { name: /pdf/i }));
    expect(await screen.findByRole('alert')).toBeInTheDocument();
  });
});

// R53: a simulated server for one human checklist step. It stores what each
// POST sends, refuses Done while an item is unticked (as the real server
// does), and lets a test hold or delay POST responses and document refetches.
type Gate = () => Promise<void>;
const open: Gate = () => Promise.resolve();
const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

function checklistServer(
  titles: string[],
  gates: { post?: Gate; doc?: Gate } = {},
): {
  stored: () => boolean[];
  posts: boolean[][];
  maxInFlight: () => number;
  inFlight: () => number;
} {
  let stored = titles.map(() => false);
  let reported = false;
  let inFlight = 0;
  let maxInFlight = 0;
  let docLoads = 0;
  const posts: boolean[][] = [];
  const snapshot = () =>
    runningDoc((d) => {
      const section = d.sections[0];
      const step = d.header.steps[0];
      if (!section || !step) return;
      step.checklist = titles.map((title, i) => ({ title, completed: stored[i] ?? false }));
      if (reported) {
        section.status = 'succeeded';
        step.status = 'succeeded';
      }
    });
  const readyWork = (sr: string) => ({
    schema_revision: 1,
    execution_id: EXE,
    completed_steps: [],
    decisions: [],
    comments: [],
    ready_work: reported
      ? []
      : [{ step_run_id: sr, path: 'smoke.login_flow', title: 'Login', actor_kind: 'human' }],
    next_action: reported ? 'wait' : 'claim_ready_work',
  });
  const problem = (code: string, status: number) =>
    HttpResponse.json(
      { code, status, detail: code, type: 't', title: 't', instance: 'urn:request:r', errors: [] },
      { status, headers: { 'Content-Type': 'application/problem+json' } },
    );
  // MSW tries handlers in list order: these overrides come before the base set.
  server.use(
    // The document is read when the request arrives and may land late.
    http.get('*/v1/executions/:exe/report', async () => {
      const body = withTabClaims(snapshot());
      docLoads += 1;
      if (docLoads > 1) await (gates.doc ?? open)();
      return HttpResponse.json(body);
    }),
    http.get('*/v1/executions/:exe/next', () =>
      HttpResponse.json(readyWork(runningDoc().sections[0]?.step_run_id ?? '')),
    ),
    http.post('*/v1/step-runs/:sr/claim', () => HttpResponse.json({ id: 'clm_1', generation: 1 })),
    http.get('*/v1/step-runs/:sr', ({ params }) =>
      HttpResponse.json({
        step_run_id: params.sr,
        instructions: 'Log in as the test user.',
        checklist: titles,
        parameters: {},
        output_schema: { type: 'object', properties: {} },
        skill: null,
      }),
    ),
    http.post('*/v1/step-runs/:sr/checklist', async ({ request }) => {
      inFlight += 1;
      maxInFlight = Math.max(maxInFlight, inFlight);
      const body = (await request.json()) as { checklist: Schemas['ChecklistTick'][] };
      const sent = body.checklist.map((tick) => tick.completed);
      stored = sent;
      posts.push(sent);
      try {
        await (gates.post ?? open)();
      } finally {
        inFlight -= 1;
      }
      return HttpResponse.json({});
    }),
    http.post('*/v1/step-runs/:sr/report', () => {
      if (!stored.every(Boolean)) return problem('step.checklist_incomplete', 409);
      reported = true;
      return HttpResponse.json({});
    }),
    http.get('*/v1/projects/:p/runbooks', () =>
      HttpResponse.json({ items: [], next_cursor: null }),
    ),
    http.get('*/v1/projects/:p/executions', () =>
      HttpResponse.json({ items: [], next_cursor: null }),
    ),
    ...baseHandlers(snapshot),
  );
  return { stored: () => stored, posts, maxInFlight: () => maxInFlight, inFlight: () => inFlight };
}

async function claimChecklistStep(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole('button', { name: /claim this step/i }));
  return screen.findByRole('group', { name: 'Checklist form' });
}

/** Mulberry32: a small seeded PRNG so a failing seed can be replayed. */
function seeded(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

describe('checklist ticks (R53)', () => {
  const titles = ['Login page loads', 'Dashboard shows data'];

  it('keeps an earlier tick when the next one lands before the document refetch', async () => {
    // Document refetches are held, so the page only has the stale (all
    // unticked) server copy while the second tick is made.
    let releaseDoc: () => void = () => {};
    const docHeld = new Promise<void>((resolve) => (releaseDoc = resolve));
    const sim = checklistServer(titles, { doc: () => docHeld });
    renderDoc();
    const user = userEvent.setup();
    const form = await claimChecklistStep(user);
    for (const title of titles) {
      const box = within(form).getByRole('checkbox', { name: title });
      await waitFor(() => expect(box).toBeEnabled());
      await user.click(box);
    }
    await waitFor(() => expect(sim.inFlight()).toBe(0));
    await waitFor(() => expect(sim.stored()).toEqual([true, true]));
    expect(sim.posts.every((sent) => sent[0] === true)).toBe(true);
    for (const title of titles) {
      expect(within(form).getByRole('checkbox', { name: title })).toBeChecked();
    }
    releaseDoc();
    await user.click(screen.getByRole('button', { name: /^done$/i }));
    await waitFor(() =>
      expect(screen.queryByRole('group', { name: 'Checklist form' })).not.toBeInTheDocument(),
    );
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('never lets a late response or refetch overwrite newer local ticks', async () => {
    // Each POST answers only when the test says so; refetches land at once.
    const pending: (() => void)[] = [];
    const sim = checklistServer(titles, {
      post: () => new Promise<void>((resolve) => pending.push(resolve)),
    });
    renderDoc();
    const user = userEvent.setup();
    const form = await claimChecklistStep(user);
    const first = within(form).getByRole('checkbox', { name: titles[0] as string });
    const second = within(form).getByRole('checkbox', { name: titles[1] as string });
    await user.click(first);
    await waitFor(() => expect(sim.posts).toHaveLength(1));
    // Tick the second while the first is still in flight.
    await waitFor(() => expect(second).toBeEnabled());
    await user.click(second);
    expect(first).toBeChecked();
    expect(second).toBeChecked();
    // Only one request at a time: the second waits for the first.
    expect(sim.posts).toHaveLength(1);
    pending.shift()?.();
    await waitFor(() => expect(sim.posts).toHaveLength(2));
    // The first response and its [true, false] refetch are older than the
    // local state: both boxes stay ticked.
    expect(first).toBeChecked();
    expect(second).toBeChecked();
    expect(sim.posts[1]).toEqual([true, true]);
    pending.shift()?.();
    await waitFor(() => expect(sim.inFlight()).toBe(0));
    expect(sim.maxInFlight()).toBe(1);
    expect(first).toBeChecked();
    expect(second).toBeChecked();
  });

  it('rolls back to the server copy on a 409 and refetches', async () => {
    let docLoads = 0;
    server.use(
      ...baseHandlers(() => {
        docLoads += 1;
        return runningDoc((d) => {
          const step = d.header.steps[0];
          if (step) step.checklist = titles.map((title) => ({ title, completed: false }));
        });
      }),
      http.get('*/v1/executions/:exe/next', () =>
        HttpResponse.json({
          ...fixtures.resumeBrief,
          ready_work: [
            {
              step_run_id: runningDoc().sections[0]?.step_run_id,
              path: 'p',
              title: 't',
              actor_kind: 'human',
            },
          ],
        }),
      ),
      http.post('*/v1/step-runs/:sr/claim', () =>
        HttpResponse.json({ id: 'clm_1', generation: 1 }),
      ),
      http.get('*/v1/step-runs/:sr', ({ params }) =>
        HttpResponse.json({
          step_run_id: params.sr,
          instructions: 'Log in.',
          checklist: titles,
          parameters: {},
          output_schema: { type: 'object', properties: {} },
          skill: null,
        }),
      ),
      // A refusal that leaves the claim in place: only the tick rolls back.
      http.post('*/v1/step-runs/:sr/checklist', () =>
        HttpResponse.json(
          {
            code: 'execution.paused',
            status: 409,
            detail: 'x',
            type: 't',
            title: 't',
            instance: 'urn:request:r',
            errors: [],
          },
          { status: 409, headers: { 'Content-Type': 'application/problem+json' } },
        ),
      ),
    );
    renderDoc();
    const user = userEvent.setup();
    const form = await claimChecklistStep(user);
    const loadsBefore = docLoads;
    const box = within(form).getByRole('checkbox', { name: titles[0] as string });
    await user.click(box);
    expect(await screen.findByText('This run is paused.')).toBeInTheDocument();
    expect(box).not.toBeChecked();
    await waitFor(() => expect(docLoads).toBeGreaterThan(loadsBefore));
  });

  // Stress: rapid ticks on three items with randomized response and
  // refetch delays. Every seed must end with the server holding exactly
  // what the person ticked, the page agreeing, and one request at a time.
  it.each([1, 2, 3, 4, 5, 6, 7, 8])(
    'stress: rapid random ticks with random delays agree with the server (seed %i)',
    async (seed) => {
      const random = seeded(seed);
      const items = ['Login page loads', 'Dashboard shows data', 'Logout works'];
      const sim = checklistServer(items, {
        post: () => sleep(Math.floor(random() * 30)),
        doc: () => sleep(Math.floor(random() * 30)),
      });
      renderDoc();
      const user = userEvent.setup();
      const form = await claimChecklistStep(user);
      const expected = items.map(() => false);
      for (let click = 0; click < 10; click += 1) {
        const index = Math.floor(random() * items.length);
        const box = within(form).getByRole('checkbox', { name: items[index] as string });
        // As fast as a person can: the next click as soon as the box is enabled.
        await waitFor(() => expect(box).toBeEnabled());
        await user.click(box);
        expected[index] = !expected[index];
        if (random() < 0.5) await sleep(Math.floor(random() * 20));
      }
      await waitFor(() => {
        expect(sim.inFlight()).toBe(0);
        expect(sim.stored()).toEqual(expected);
      });
      // Let any trailing refetch land; it must not undo a tick.
      await sleep(60);
      items.forEach((title, i) => {
        const box = within(form).getByRole('checkbox', { name: title });
        if (expected[i]) expect(box).toBeChecked();
        else expect(box).not.toBeChecked();
      });
      expect(sim.maxInFlight()).toBe(1);
      cleanup();
    },
  );
});

// R55: "Claim this step" only where a person may claim: a ready task whose
// actor is human or any, or a task handed off to a person. The human
// audience's resume brief (GET /v1/executions/{exe}/next) is the server's
// own list of that work; the document alone does not carry the declared
// actor or the handoff.
describe('claim is offered only where a person may claim (R55)', () => {
  type Case = {
    name: string;
    shape: (section: Doc['sections'][number], step: Doc['header']['steps'][number]) => void;
    brief: 'human' | 'agent' | 'absent';
    claim: boolean;
  };
  const task: Case['shape'] = (section, step) => {
    section.kind = 'task';
    step.kind = 'task';
  };
  const cases: Case[] = [
    {
      name: 'agent-only task (not in the human brief)',
      shape: task,
      brief: 'absent',
      claim: false,
    },
    { name: 'agent-only task listed as agent work', shape: task, brief: 'agent', claim: false },
    { name: 'human task', shape: task, brief: 'human', claim: true },
    { name: 'any-actor task (human work for a person)', shape: task, brief: 'human', claim: true },
    {
      name: 'agent task handed off to a person',
      shape: (section, step) => {
        task(section, step);
        step.attempt = 2;
        step.eligible_recipients = [{ kind: 'team', reference: 'oncall' }];
      },
      brief: 'human',
      claim: true,
    },
    {
      name: 'human task that is no longer ready (stale brief)',
      shape: (section, step) => {
        task(section, step);
        section.status = 'claimed';
        step.status = 'claimed';
        section.actor = { kind: 'human', user_id: 'usr_other' };
        step.actor = { kind: 'human', user_id: 'usr_other' };
      },
      brief: 'human',
      claim: false,
    },
    {
      name: 'approval (decided in the decision panel)',
      shape: (section, step) => {
        section.kind = 'approval';
        step.kind = 'approval';
        section.approval = {
          schema_revision: 1,
          step_run_id: section.step_run_id,
          status: 'waiting',
          prompt: 'Roll back or keep?',
          options: [{ id: 'keep', title: 'Keep deployment', auto: false }],
          eligible_recipients: [{ kind: 'team', reference: 'oncall' }],
          escalation_recipients: [],
          escalated: false,
          quorum: 1,
          decisions: [],
        };
      },
      brief: 'human',
      claim: false,
    },
  ];

  it.each(cases)('$name: claim offered = $claim', async ({ shape, brief, claim }) => {
    const doc = runningDoc((d) => {
      const [first, second] = d.sections;
      const [firstStep, secondStep] = d.header.steps;
      if (!first || !second || !firstStep || !secondStep) return;
      shape(first, firstStep);
      // A control step a person may claim: once its button shows, the brief
      // has loaded, so a missing button on the first step is meaningful.
      second.status = 'ready';
      second.kind = 'task';
      second.actor = null;
      second.approval = null;
      secondStep.status = 'ready';
      secondStep.kind = 'task';
      secondStep.actor = null;
      secondStep.claim = null;
    });
    const [first, second] = doc.sections;
    const ready: { sr: string; actor?: 'human' | 'agent' }[] = [{ sr: second?.step_run_id ?? '' }];
    if (brief !== 'absent') ready.unshift({ sr: first?.step_run_id ?? '', actor: brief });
    let claims = 0;
    server.use(
      http.get('*/v1/executions/:exe/next', () => HttpResponse.json(briefFor(ready))),
      http.post('*/v1/step-runs/:sr/claim', () => {
        claims += 1;
        return HttpResponse.json({ id: 'clm_1', generation: 1 });
      }),
      ...baseHandlers(doc),
    );
    renderDoc();
    const control = await screen.findByTestId(`actions-${second?.step_run_id}`);
    expect(
      await within(control).findByRole('button', { name: 'Claim this step' }),
    ).toBeInTheDocument();
    const firstActions = screen.queryByTestId(`actions-${first?.step_run_id}`);
    const firstClaim = firstActions
      ? within(firstActions).queryByRole('button', { name: 'Claim this step' })
      : null;
    if (claim) {
      expect(firstClaim).toBeInTheDocument();
    } else {
      expect(firstClaim).not.toBeInTheDocument();
    }
    expect(claims).toBe(0);
  });
});

// U2 (D20): one idempotency key per user action, reused when the person
// retries it after a lost response, and a new key once the action has a
// definitive outcome. Each handler drops the first response (the server
// committed, the proxy reset) and answers the retry.
describe('one idempotency key per user action across retries (U2)', () => {
  /** Records each request's Idempotency-Key; the first `drops` get a network error. */
  function flaky(respond: () => Response, drops = 1) {
    const keys: (string | null)[] = [];
    const handler = ({ request }: { request: Request }) => {
      keys.push(request.headers.get('idempotency-key'));
      return keys.length <= drops ? HttpResponse.error() : respond();
    };
    return { keys, handler };
  }
  const claimRead = () => [
    http.post('*/v1/step-runs/:sr/claim', () => HttpResponse.json({ id: 'clm_1', generation: 1 })),
    http.get('*/v1/step-runs/:sr', ({ params }) =>
      HttpResponse.json({
        step_run_id: params.sr,
        instructions: 'Call /healthz and record the status.',
        checklist: ['Login page loads'],
        parameters: {},
        output_schema: { type: 'object', properties: {} },
        skill: null,
        template: { instructions: 'Call /healthz.', checklist: ['Login page loads'], values: {} },
      }),
    ),
  ];
  const expectOneKey = (keys: (string | null)[], attempts = 2) => {
    expect(keys).toHaveLength(attempts);
    expect(keys[0]).toBeTruthy();
    for (const key of keys) expect(key).toBe(keys[0]);
  };

  it('Rerun', async () => {
    const doc = runningDoc((d) => {
      d.header.status = 'failed';
      d.header.phase = 'completed';
    });
    const rerun = flaky(() =>
      HttpResponse.json({ ...doc.header, id: 'exe_01JZZZZZZZZZZZZZZZZZZZZZZZ' }),
    );
    server.use(...baseHandlers(doc), http.post('*/v1/executions/:exe/rerun', rerun.handler));
    renderDoc();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /^rerun$/i }));
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: /^rerun$/i }));
    await waitFor(() => expect(rerun.keys).toHaveLength(2));
    expectOneKey(rerun.keys);
  });

  it('Send signal, and a new key for a different signal', async () => {
    const doc = runningDoc((d) => {
      const section = d.sections[0];
      if (section) {
        section.kind = 'wait';
        section.status = 'waiting';
      }
    });
    const signal = flaky(() => HttpResponse.json({}));
    server.use(
      ...baseHandlers(doc),
      http.post('*/v1/executions/:exe/signals/:name', signal.handler),
    );
    renderDoc();
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText('Signal name'), 'deploy-done');
    await user.click(screen.getByRole('button', { name: /send signal/i }));
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: /send signal/i }));
    await waitFor(() => expect(signal.keys).toHaveLength(2));
    expectOneKey(signal.keys);
    // A success ends the action: sending again is a new action.
    await user.click(screen.getByRole('button', { name: /send signal/i }));
    await waitFor(() => expect(signal.keys).toHaveLength(3));
    expect(signal.keys[2]).not.toBe(signal.keys[0]);
  });

  it('Report (Done)', async () => {
    const report = flaky(() => HttpResponse.json({}));
    server.use(
      ...baseHandlers(runningDoc()),
      ...claimRead(),
      http.post('*/v1/step-runs/:sr/report', report.handler),
    );
    renderDoc();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /claim this step/i }));
    await user.click(await screen.findByRole('button', { name: /^done$/i }));
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: /^done$/i }));
    await waitFor(() => expect(report.keys).toHaveLength(2));
    expectOneKey(report.keys);
  });

  it('Release claim', async () => {
    const release = flaky(() => HttpResponse.json({}));
    server.use(
      ...baseHandlers(runningDoc()),
      ...claimRead(),
      http.post('*/v1/step-runs/:sr/release', release.handler),
    );
    renderDoc();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /claim this step/i }));
    await user.click(await screen.findByRole('button', { name: /release claim/i }));
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: /release claim/i }));
    await waitFor(() => expect(release.keys).toHaveLength(2));
    expectOneKey(release.keys);
  });

  it('Checklist tick', async () => {
    const checklist = flaky(() => HttpResponse.json({}));
    server.use(
      ...baseHandlers(runningDoc()),
      ...claimRead(),
      http.post('*/v1/step-runs/:sr/checklist', checklist.handler),
    );
    renderDoc();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /claim this step/i }));
    const box = await screen.findByRole('checkbox', { name: /login page loads/i });
    await user.click(box);
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    await waitFor(() => expect(box).not.toBeChecked());
    await user.click(box);
    await waitFor(() => expect(checklist.keys).toHaveLength(2));
    expectOneKey(checklist.keys);
  });

  it('Decide: the same option reuses its key; a definitive refusal ends the action', async () => {
    const doc = runningDoc((d) => {
      const section = d.sections[0];
      if (section) {
        section.status = 'waiting';
        section.approval = {
          schema_revision: 1,
          step_run_id: section.step_run_id,
          status: 'waiting',
          prompt: 'Checks failed. Choose an action.',
          options: [{ id: 'keep', title: 'Keep deployment', auto: false }],
          eligible_recipients: [{ kind: 'team', reference: 'oncall' }],
          escalation_recipients: [],
          escalated: false,
          quorum: 1,
          decisions: [],
        };
      }
    });
    const keys: (string | null)[] = [];
    server.use(
      ...baseHandlers(doc),
      http.post('*/v1/step-runs/:sr/decisions', ({ request }) => {
        keys.push(request.headers.get('idempotency-key'));
        if (keys.length === 1) return HttpResponse.error();
        return HttpResponse.json(
          {
            code: 'approval.not_open',
            status: 409,
            detail: 'x',
            type: 't',
            title: 't',
            instance: 'urn:request:r',
            errors: [],
          },
          { status: 409, headers: { 'Content-Type': 'application/problem+json' } },
        );
      }),
    );
    renderDoc();
    const user = userEvent.setup();
    const actions = await screen.findByTestId(/actions-/);
    const vote = async () => {
      await user.click(within(actions).getByRole('button', { name: /keep deployment/i }));
      const confirm = screen.queryByRole('alertdialog');
      if (confirm)
        await user.click(within(confirm).getByRole('button', { name: /keep deployment/i }));
    };
    await vote();
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    await vote();
    await waitFor(() => expect(keys).toHaveLength(2));
    expectOneKey(keys);
    expect(await screen.findByText('This approval has closed.')).toBeInTheDocument();
    // 409 is definitive: trying again is a new action with a new key.
    await vote();
    await waitFor(() => expect(keys).toHaveLength(3));
    expect(keys[2]).not.toBe(keys[0]);
  });
});

// U5: the claims this tab remembers (sessionStorage) are reconciled with
// every document the page reads; a claim the document no longer shows as
// this tab's is dropped, with its form and cached step read.
describe('remembered claims follow the document (U5)', () => {
  const SR = () => runningDoc().sections[0]?.step_run_id ?? '';
  const stepRead = () =>
    http.get('*/v1/step-runs/:sr', ({ params }) =>
      HttpResponse.json({
        step_run_id: params.sr,
        instructions: 'Call /healthz and record the status.',
        checklist: [],
        parameters: {},
        output_schema: { type: 'object', properties: {} },
        skill: null,
        template: { instructions: 'Call /healthz.', checklist: [], values: {} },
      }),
    );
  const claimedBy = (claim: Schemas['ClaimRef']) => (d: Doc) => {
    const section = d.sections[0];
    const step = d.header.steps[0];
    if (!section || !step) return;
    section.status = 'claimed';
    section.actor = { kind: 'human', user_id: 'usr_1' };
    step.status = 'claimed';
    step.actor = { kind: 'human', user_id: 'usr_1' };
    step.claim = claim;
  };

  it('drops a claim released in another tab (F100 "Release it here") and offers Claim again', async () => {
    tabClaims.set(SR(), { id: 'clm_1', generation: 1 });
    server.use(...baseHandlers(runningDoc(), { reflectTabClaims: false }), stepRead());
    renderDoc();
    const actions = await screen.findByTestId(`actions-${SR()}`);
    expect(
      await within(actions).findByRole('button', { name: 'Claim this step' }),
    ).toBeInTheDocument();
    expect(
      within(actions).queryByRole('button', { name: /release claim/i }),
    ).not.toBeInTheDocument();
    expect(within(actions).queryByText(/call \/healthz/i)).not.toBeInTheDocument();
    expect(tabClaims.get(SR())).toBeUndefined();
  });

  it('shows "claimed by you elsewhere" when another tab re-claimed the step', async () => {
    tabClaims.set(SR(), { id: 'clm_1', generation: 1 });
    server.use(
      ...baseHandlers(runningDoc(claimedBy({ id: 'clm_2', generation: 2 })), {
        reflectTabClaims: false,
      }),
      stepRead(),
    );
    renderDoc();
    expect(await screen.findByText(/claimed by you elsewhere/i)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /release it here/i })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^done$/i })).not.toBeInTheDocument();
    expect(tabClaims.get(SR())).toBeUndefined();
  });

  it('keeps a claim the document shows as this tab’s', async () => {
    tabClaims.set(SR(), { id: 'clm_1', generation: 1 });
    server.use(...baseHandlers(runningDoc()), stepRead());
    renderDoc();
    expect(await screen.findByText(/call \/healthz/i)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /^done$/i })).toBeInTheDocument();
    expect(tabClaims.get(SR())).toEqual({ id: 'clm_1', generation: 1 });
  });

  it('this tab releases a claim held elsewhere, and Claim returns once the document shows it free', async () => {
    let released = false;
    const doc = () =>
      released ? runningDoc() : runningDoc(claimedBy({ id: 'clm_other', generation: 2 }));
    server.use(
      ...baseHandlers(doc),
      http.post('*/v1/step-runs/:sr/release', () => {
        released = true;
        return HttpResponse.json({});
      }),
    );
    renderDoc();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /release it here/i }));
    expect(await screen.findByRole('button', { name: 'Claim this step' })).toBeInTheDocument();
    expect(screen.queryByText(/claimed by you elsewhere/i)).not.toBeInTheDocument();
  });

  it('drops the report form when the report answers 409 step.completed', async () => {
    let completed = false;
    const doc = () =>
      runningDoc((d) => {
        const section = d.sections[0];
        const step = d.header.steps[0];
        if (completed && section && step) {
          section.status = 'succeeded';
          step.status = 'succeeded';
        }
      });
    server.use(
      ...baseHandlers(doc),
      stepRead(),
      http.post('*/v1/step-runs/:sr/claim', () =>
        HttpResponse.json({ id: 'clm_1', generation: 1 }),
      ),
      http.post('*/v1/step-runs/:sr/report', () => {
        completed = true;
        return HttpResponse.json(
          {
            code: 'step.completed',
            status: 409,
            detail: 'x',
            type: 't',
            title: 't',
            instance: 'urn:request:r',
            errors: [],
          },
          { status: 409, headers: { 'Content-Type': 'application/problem+json' } },
        );
      }),
    );
    renderDoc();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /claim this step/i }));
    await user.click(await screen.findByRole('button', { name: /^done$/i }));
    expect(await screen.findByText('That step has already completed.')).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.queryByRole('button', { name: /^done$/i })).not.toBeInTheDocument(),
    );
    expect(tabClaims.get(SR())).toBeUndefined();
  });

  it('drops the claim when the run is cancelled while the step is claimed', async () => {
    tabClaims.set(SR(), { id: 'clm_1', generation: 1 });
    const doc = runningDoc((d) => {
      d.header.status = 'cancelled';
      d.header.phase = 'completed';
      const section = d.sections[0];
      const step = d.header.steps[0];
      if (section && step) {
        section.status = 'cancelled';
        step.status = 'cancelled';
      }
    });
    server.use(...baseHandlers(doc), stepRead());
    renderDoc();
    expect(
      await screen.findByRole('heading', { name: /health endpoint returns 200/i }),
    ).toBeInTheDocument();
    await waitFor(() => expect(tabClaims.get(SR())).toBeUndefined());
    expect(screen.queryByRole('button', { name: /^done$/i })).not.toBeInTheDocument();
  });
});

// U4 (D27, PRD §22): pause, resume and cancel need project role operator or
// above AND the executions:admin scope on the token. The page offers them
// only then; otherwise it says why, or (no role) shows nothing.
describe('pause, resume and cancel follow scope and project role (U4)', () => {
  const ADMIN = ['executions:admin'];
  const problem = (code: string, status: number) =>
    HttpResponse.json(
      { code, status, detail: 'x', type: 't', title: 't', instance: 'urn:request:r', errors: [] },
      { status, headers: { 'Content-Type': 'application/problem+json' } },
    );

  it('offers Pause and Cancel to an operator whose token has executions:admin; Cancel confirms and PATCHes', async () => {
    const patches: unknown[] = [];
    server.use(
      ...baseHandlers(runningDoc(), { extraScopes: ADMIN }),
      http.patch('*/v1/executions/:exe', async ({ request }) => {
        patches.push(await request.json());
        return HttpResponse.json({});
      }),
    );
    renderDoc();
    const user = userEvent.setup();
    expect(await screen.findByRole('button', { name: /^pause$/i })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: /^cancel run$/i }));
    const dialog = await screen.findByRole('alertdialog');
    await user.click(within(dialog).getByRole('button', { name: /^cancel run$/i }));
    await waitFor(() => expect(patches).toEqual([{ action: 'cancel' }]));
  });

  it('offers Resume on a paused run', async () => {
    server.use(
      ...baseHandlers(
        runningDoc((d) => {
          d.header.status = 'paused';
        }),
        { extraScopes: ADMIN, projectRole: 'admin' },
      ),
    );
    renderDoc();
    expect(await screen.findByRole('button', { name: /^resume$/i })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^pause$/i })).not.toBeInTheDocument();
  });

  it('explains instead of offering controls when the token lacks executions:admin (even for an org owner)', async () => {
    server.use(...baseHandlers(runningDoc(), { projectRole: 'operator' }));
    renderDoc();
    expect(
      await screen.findByText(
        /pause, resume and cancel need a token with the executions:admin scope/i,
      ),
    ).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^pause$/i })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^cancel run$/i })).not.toBeInTheDocument();
  });

  it('offers no controls, and no Claim, to a project viewer even with executions:admin', async () => {
    server.use(...baseHandlers(runningDoc(), { extraScopes: ADMIN, projectRole: 'viewer' }));
    renderDoc();
    expect(
      await screen.findByRole('heading', { name: /health endpoint returns 200/i }),
    ).toBeInTheDocument();
    // The members read has landed once the role-based hint can be decided.
    await waitFor(() => expect(screen.getByRole('button', { name: /^export$/i })).toBeEnabled());
    await new Promise((r) => setTimeout(r, 50));
    expect(screen.queryByRole('button', { name: /^pause$/i })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^cancel run$/i })).not.toBeInTheDocument();
    expect(screen.queryByText(/executions:admin/i)).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /claim this step/i })).not.toBeInTheDocument();
  });

  it('maps 403 auth.scope_missing to a plain explanation', async () => {
    server.use(
      ...baseHandlers(runningDoc(), { extraScopes: ADMIN }),
      http.patch('*/v1/executions/:exe', () => problem('auth.scope_missing', 403)),
    );
    renderDoc();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /^pause$/i }));
    expect(await screen.findByRole('alert')).toHaveTextContent(
      /needs a token with the executions:admin scope/i,
    );
  });

  it('counts a cancel retry that finds the run closed (first response lost) as done', async () => {
    let calls = 0;
    server.use(
      ...baseHandlers(runningDoc(), { extraScopes: ADMIN }),
      http.patch('*/v1/executions/:exe', () => {
        calls += 1;
        return calls === 1 ? HttpResponse.error() : problem('execution.closed', 409);
      }),
    );
    renderDoc();
    const user = userEvent.setup();
    const cancel = async () => {
      await user.click(await screen.findByRole('button', { name: /^cancel run$/i }));
      const dialog = await screen.findByRole('alertdialog');
      await user.click(within(dialog).getByRole('button', { name: /^cancel run$/i }));
    };
    await cancel();
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    await cancel();
    await waitFor(() => expect(calls).toBe(2));
    await waitFor(() => expect(screen.queryByRole('alert')).not.toBeInTheDocument());
  });
});

// U8: one report: outputs, summary and verdict go in one POST from Done,
// and 422 field errors land on the fields they name.
describe('report outputs and a summary together (U8)', () => {
  const OUTPUT_SCHEMA = {
    type: 'object',
    properties: { error_rate: { type: 'number', title: 'Error rate' } },
    required: ['error_rate'],
  };
  function reportServer(respond: () => Response = () => HttpResponse.json({})) {
    const bodies: Record<string, unknown>[] = [];
    server.use(
      ...baseHandlers(runningDoc()),
      http.post('*/v1/step-runs/:sr/claim', () =>
        HttpResponse.json({ id: 'clm_1', generation: 1 }),
      ),
      http.get('*/v1/step-runs/:sr', ({ params }) =>
        HttpResponse.json({
          step_run_id: params.sr,
          instructions: 'Read the error rate from the dashboard.',
          checklist: [],
          parameters: {},
          output_schema: OUTPUT_SCHEMA,
          skill: null,
          template: { instructions: 'Read the error rate.', checklist: [], values: {} },
        }),
      ),
      http.post('*/v1/step-runs/:sr/report', async ({ request }) => {
        bodies.push((await request.json()) as Record<string, unknown>);
        return respond();
      }),
    );
    return bodies;
  }

  it('Done sends the outputs and the summary in one report', async () => {
    const bodies = reportServer();
    renderDoc();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /claim this step/i }));
    await user.type(await screen.findByRole('textbox', { name: /error rate/i }), '0.02');
    await user.type(screen.getByRole('textbox', { name: /^summary/i }), 'Dashboard green');
    await user.selectOptions(screen.getByLabelText(/check verdict/i), 'pass');
    expect(screen.getAllByRole('button', { name: /^done$/i })).toHaveLength(1);
    await user.click(screen.getByRole('button', { name: /^done$/i }));
    await waitFor(() => expect(bodies).toHaveLength(1));
    expect(bodies[0]).toMatchObject({
      status: 'succeeded',
      outputs: { error_rate: 0.02 },
      summary: 'Dashboard green',
      verdict: 'pass',
    });
  });

  it('marks the field a 422 names', async () => {
    reportServer(() =>
      HttpResponse.json(
        {
          code: 'step.invalid_outputs',
          status: 422,
          detail: 'x',
          type: 't',
          title: 't',
          instance: 'urn:request:r',
          errors: [
            {
              path: 'outputs.error_rate',
              code: 'data.maximum',
              message: 'Output does not satisfy its declaration.',
            },
          ],
        },
        { status: 422, headers: { 'Content-Type': 'application/problem+json' } },
      ),
    );
    renderDoc();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /claim this step/i }));
    await user.type(await screen.findByRole('textbox', { name: /error rate/i }), '7');
    await user.click(screen.getByRole('button', { name: /^done$/i }));
    expect(await screen.findByText('Output does not satisfy its declaration.')).toBeInTheDocument();
    expect(screen.getByRole('alert')).toHaveTextContent(/outputs.*marked/i);
    // The form keeps what the person typed.
    expect(screen.getByRole('textbox', { name: /error rate/i })).toHaveValue('7');
  });

  it('Skip asks for the reason the server requires and sends it', async () => {
    const bodies = reportServer();
    renderDoc();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /claim this step/i }));
    await user.click(await screen.findByRole('button', { name: /^skip$/i }));
    const dialog = await screen.findByRole('dialog');
    await user.type(within(dialog).getByRole('textbox'), 'Dashboard retired');
    await user.click(within(dialog).getByRole('button', { name: /submit report/i }));
    await waitFor(() => expect(bodies).toHaveLength(1));
    expect(bodies[0]).toMatchObject({ status: 'skipped', summary: 'Dashboard retired' });
  });
});

// U11: focus follows the person's action, live changes are announced
// politely, and the page title names the run and its status.
describe('focus, announcements and title (U11)', () => {
  const SR = () => runningDoc().sections[0]?.step_run_id ?? '';
  const heading = (sr: string) =>
    within(document.querySelector(`section[data-step-id="${sr}"]`) as HTMLElement).getByRole(
      'heading',
      { level: 2 },
    );
  const claimAndRead = () => [
    http.post('*/v1/step-runs/:sr/claim', () => HttpResponse.json({ id: 'clm_1', generation: 1 })),
    http.get('*/v1/step-runs/:sr', ({ params }) =>
      HttpResponse.json({
        step_run_id: params.sr,
        instructions: 'Call /healthz and record the status.',
        checklist: [],
        parameters: {},
        output_schema: { type: 'object', properties: {} },
        skill: null,
        template: { instructions: 'Call /healthz.', checklist: [], values: {} },
      }),
    ),
  ];

  it('moves focus to the step heading after claim, report and release', async () => {
    let reported = false;
    const doc = () =>
      runningDoc((d) => {
        const section = d.sections[0];
        const step = d.header.steps[0];
        if (reported && section && step) {
          section.status = 'succeeded';
          step.status = 'succeeded';
        }
      });
    server.use(
      ...baseHandlers(doc),
      ...claimAndRead(),
      http.post('*/v1/step-runs/:sr/release', () => HttpResponse.json({})),
      http.post('*/v1/step-runs/:sr/report', () => {
        reported = true;
        return HttpResponse.json({});
      }),
    );
    renderDoc();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /claim this step/i }));
    await waitFor(() => expect(document.activeElement).toBe(heading(SR())));

    await user.click(await screen.findByRole('button', { name: /release claim/i }));
    await waitFor(() => expect(document.activeElement).toBe(heading(SR())));

    await user.click(await screen.findByRole('button', { name: /claim this step/i }));
    await user.click(await screen.findByRole('button', { name: /^done$/i }));
    await waitFor(() => expect(document.activeElement).toBe(heading(SR())));
    await waitFor(() =>
      expect(screen.queryByRole('button', { name: /^done$/i })).not.toBeInTheDocument(),
    );
  });

  it('activating a step in the rail moves focus to its section; arrow keys stay in the rail', async () => {
    server.use(...baseHandlers(runningDoc()));
    renderDoc();
    const user = userEvent.setup();
    const rail = await screen.findByRole('navigation', { name: /steps/i });
    const second = runningDoc().sections[1]?.step_run_id ?? '';
    const items = within(rail).getAllByRole('button');
    await user.click(items[1] as HTMLElement);
    await waitFor(() => expect(document.activeElement).toBe(heading(second)));

    (items[0] as HTMLElement).focus();
    await user.keyboard('{ArrowDown}');
    expect(document.activeElement).toBe(items[1]);
  });

  it('announces a step’s status change from a live event in a polite log', async () => {
    // The real StepReported frame from the server triggers the refetch.
    const reported = CANCELLED_RUN_STREAM.split(/(?<=\n\n)/).find((b) => b.startsWith('id: 8\n'));
    let served = false;
    const doc = () =>
      runningDoc((d) => {
        const section = d.sections[0];
        const step = d.header.steps[0];
        if (served && section && step) {
          section.status = 'succeeded';
          step.status = 'succeeded';
        }
      });
    server.use(
      http.get('*/v1/executions/:exe/events', () => {
        if (served) {
          return new HttpResponse(new ReadableStream<Uint8Array>({ start() {} }), {
            headers: { 'Content-Type': 'text/event-stream' },
          });
        }
        served = true;
        return HttpResponse.text(reported ?? '', {
          headers: { 'Content-Type': 'text/event-stream' },
        });
      }),
      ...baseHandlers(doc),
    );
    renderDoc();
    const log = await screen.findByRole('log');
    expect(log).toHaveAttribute('aria-live', 'polite');
    await waitFor(
      () => expect(log).toHaveTextContent('Step 1, Health endpoint returns 200: succeeded'),
      { timeout: 4000 },
    );
  });

  it('titles the page with the run title and status', async () => {
    server.use(...baseHandlers(runningDoc()));
    const view = renderDoc();
    await waitFor(() =>
      expect(document.title).toBe('Post-deploy verification (running) · The Runbook'),
    );
    view.unmount();
  });
});

// U16: what the decision panel says depends on the approval's state.
describe('your turn callout (review B)', () => {
  it('offers the waiting approval above the tabs when it is yours', async () => {
    const doc = runningDoc((d) => {
      const section = d.sections[0];
      if (section) {
        section.status = 'waiting';
        section.approval = {
          schema_revision: 1,
          step_run_id: section.step_run_id,
          status: 'waiting',
          prompt: 'Roll back or keep?',
          options: [{ id: 'keep', title: 'Keep deployment', auto: false }],
          eligible_recipients: [{ kind: 'team', reference: 'oncall' }],
          escalation_recipients: [],
          escalated: false,
          quorum: 1,
          decisions: [],
        };
      }
    });
    server.use(...baseHandlers(doc));
    renderDoc();
    const callout = await screen.findByRole('region', { name: /your turn/i });
    expect(callout).toHaveTextContent(/is waiting for you/i);
    expect(screen.getByRole('button', { name: /review the approval/i })).toBeInTheDocument();
  });
});

describe('approval state on the page (U16)', () => {
  function approvalDoc(status: Schemas['StepStatus'], decisions: Schemas['DecisionRecord'][] = []) {
    return runningDoc((d) => {
      const section = d.sections[0];
      if (section) {
        section.status = status;
        section.approval = {
          schema_revision: 1,
          step_run_id: section.step_run_id,
          status,
          prompt: 'Roll back or keep?',
          options: [{ id: 'keep', title: 'Keep deployment', auto: false }],
          eligible_recipients: [{ kind: 'team', reference: 'oncall' }],
          escalation_recipients: [],
          escalated: false,
          quorum: 2,
          decisions,
        };
      }
    });
  }

  it('says a decided approval is closed, not that the person is ineligible', async () => {
    server.use(...baseHandlers(approvalDoc('succeeded')));
    renderDoc();
    const actions = await screen.findByTestId(/actions-/);
    expect(await within(actions).findByText(/^closed \(succeeded\)/i)).toBeInTheDocument();
    expect(within(actions).queryByText(/not eligible/i)).not.toBeInTheDocument();
    expect(
      within(actions).queryByRole('button', { name: /keep deployment/i }),
    ).not.toBeInTheDocument();
  });

  it('#5 says what was decided, by whom, from the step outcome; no call to action', async () => {
    const DANA = 'usr_01M76R2QSVVFRN94EPY96GDTND';
    const doc = approvalDoc('succeeded', [
      { by: 'human', user_id: DANA, membership_revision: 1, option: 'keep' },
    ]);
    const section = doc.sections[0];
    if (section) section.outputs = { decision: 'keep', decided_by: DANA, comment: 'fine' };
    server.use(...baseHandlers(doc));
    server.use(
      http.get('*/v1/orgs/:org/users', () =>
        HttpResponse.json({
          items: [{ id: DANA, name: 'Dana Directory', role: 'member', active: true }],
          next_cursor: null,
        }),
      ),
    );
    renderDoc();
    const actions = await screen.findByTestId(/actions-/);
    expect(
      await within(actions).findByText('Decided: Keep deployment by Dana Directory'),
    ).toBeInTheDocument();
    expect(within(actions).queryByText(/approval required/i)).not.toBeInTheDocument();
    expect(within(actions).queryByText('Roll back or keep?')).not.toBeInTheDocument();
    expect(within(actions).queryByRole('button')).not.toBeInTheDocument();
  });

  it('#7 says "not eligible" inside the approval panel', async () => {
    const doc = approvalDoc('waiting');
    const section = doc.sections[0];
    if (section?.approval)
      section.approval.eligible_recipients = [{ kind: 'team', reference: 'leads' }];
    server.use(...baseHandlers(doc));
    renderDoc();
    const notice = await screen.findByText(/not eligible to decide/i);
    const panel = notice.closest('[data-slot="approval-panel"]');
    expect(panel).not.toBeNull();
    expect(
      within(panel as HTMLElement).getByRole('button', { name: /keep deployment/i }),
    ).toBeDisabled();
  });

  it('tells a person who already voted that their vote is in', async () => {
    server.use(
      ...baseHandlers(
        approvalDoc('waiting', [
          { by: 'human', user_id: 'usr_1', membership_revision: 1, option: 'keep' },
        ]),
      ),
    );
    renderDoc();
    const actions = await screen.findByTestId(/actions-/);
    expect(await within(actions).findByText(/you have voted/i)).toBeInTheDocument();
    expect(within(actions).queryByText(/not eligible/i)).not.toBeInTheDocument();
  });
});

// U17: log reads show their errors, and an active step's log follows the run.
describe('step logs (U17)', () => {
  const problem = (code: string, status: number) =>
    HttpResponse.json(
      { code, status, detail: 'x', type: 't', title: 't', instance: 'urn:request:r', errors: [] },
      { status, headers: { 'Content-Type': 'application/problem+json' } },
    );

  it('shows a refused log read as an error, not as an empty log', async () => {
    server.use(
      ...baseHandlers(runningDoc()),
      http.get('*/v1/step-runs/:sr/logs', () => problem('auth.forbidden', 403)),
    );
    renderDoc();
    const user = userEvent.setup();
    const showers = await screen.findAllByRole('button', { name: /show log/i });
    await user.click(showers[0] as HTMLElement);
    expect(
      await screen.findByText(/the log could not be loaded/i, {}, { timeout: 4000 }),
    ).toBeInTheDocument();
    expect(screen.queryByText(/no logs recorded/i)).not.toBeInTheDocument();
  });

  it('refetches an open log when a live event refreshes an active step', async () => {
    // The step is claimed (an agent is working); a real StepReported frame
    // from the server triggers the document refetch.
    const frame = CANCELLED_RUN_STREAM.split(/(?<=\n\n)/).find((b) => b.startsWith('id: 8\n'));
    let lines = ['first'];
    let releaseEvent: () => void = () => {};
    const eventGate = new Promise<void>((r) => (releaseEvent = r));
    let served = false;
    server.use(
      http.get('*/v1/executions/:exe/events', async () => {
        if (served) {
          return new HttpResponse(new ReadableStream<Uint8Array>({ start() {} }), {
            headers: { 'Content-Type': 'text/event-stream' },
          });
        }
        served = true;
        await eventGate;
        return HttpResponse.text(frame ?? '', { headers: { 'Content-Type': 'text/event-stream' } });
      }),
      http.get('*/v1/step-runs/:sr/logs', () =>
        HttpResponse.json({
          lines: lines.map((line, i) => ({ seq: i + 1, line })),
          next_cursor: String(lines.length),
          has_more: false,
        }),
      ),
      ...baseHandlers(
        runningDoc((d) => {
          const section = d.sections[0];
          if (section) {
            section.status = 'claimed';
            section.actor = {
              kind: 'agent',
              principal_id: 'p',
              session_id: 's',
              name: 'bot',
              model: 'm',
            };
          }
        }),
      ),
    );
    renderDoc();
    const user = userEvent.setup();
    const showers = await screen.findAllByRole('button', { name: /show log/i });
    await user.click(showers[0] as HTMLElement);
    expect(await screen.findByText('first')).toBeInTheDocument();
    lines = ['first', 'second'];
    releaseEvent();
    expect(await screen.findByText('second', {}, { timeout: 4000 })).toBeInTheDocument();
  });
});

// U18: small execution-page items.
describe('small execution-page items (U18)', () => {
  it('revokes the Markdown blob URL only after the download has started', async () => {
    server.use(...baseHandlers(runningDoc()));
    const revoke = vi.fn();
    URL.createObjectURL = vi.fn(() => 'blob:report');
    URL.revokeObjectURL = revoke;
    let revokedAtClick: boolean | null = null;
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function () {
      // Safari reads the URL after click() returns: it must still be valid.
      queueMicrotask(() => {
        revokedAtClick = revoke.mock.calls.length > 0;
      });
    });
    renderDoc();
    const user = userEvent.setup();
    await exportMarkdown(user);
    await waitFor(() => expect(click).toHaveBeenCalled());
    await waitFor(() => expect(revokedAtClick).toBe(false));
    await waitFor(() => expect(revoke).toHaveBeenCalledWith('blob:report'), { timeout: 3000 });
    click.mockRestore();
  });

  it('matches checklist ticks by position, so repeated titles tick separately', async () => {
    const titles = ['Check the graph', 'Check the graph'];
    server.use(
      ...baseHandlers(
        runningDoc((d) => {
          const step = d.header.steps[0];
          if (step) {
            step.checklist = [
              { title: 'Check the graph', completed: true },
              { title: 'Check the graph', completed: false },
            ];
          }
        }),
      ),
      http.post('*/v1/step-runs/:sr/claim', () =>
        HttpResponse.json({ id: 'clm_1', generation: 1 }),
      ),
      http.get('*/v1/step-runs/:sr', ({ params }) =>
        HttpResponse.json({
          step_run_id: params.sr,
          instructions: 'Look twice.',
          checklist: titles,
          parameters: {},
          output_schema: { type: 'object', properties: {} },
          skill: null,
          template: { instructions: 'Look twice.', checklist: titles, values: {} },
        }),
      ),
    );
    renderDoc();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /claim this step/i }));
    const form = await screen.findByRole('group', { name: 'Checklist form' });
    const boxes = within(form).getAllByRole('checkbox');
    expect(boxes[0]).toBeChecked();
    expect(boxes[1]).not.toBeChecked();
  });

  it('resumes the stream after the last event this tab saw when the page is opened again', async () => {
    const seen: (string | null)[] = [];
    const frame = CANCELLED_RUN_STREAM.split(/(?<=\n\n)/).find((b) => b.startsWith('id: 8\n'));
    const encoder = new TextEncoder();
    server.use(
      http.get('*/v1/executions/:exe/events', ({ request }) => {
        seen.push(request.headers.get('last-event-id'));
        return new HttpResponse(
          new ReadableStream<Uint8Array>({
            start(controller) {
              if (seen.length === 1) controller.enqueue(encoder.encode(frame ?? ''));
            },
          }),
          { headers: { 'Content-Type': 'text/event-stream' } },
        );
      }),
      ...baseHandlers(runningDoc()),
    );
    const first = renderDoc();
    await waitFor(() => expect(seen).toHaveLength(1));
    await screen.findByText('Live');
    await new Promise((r) => setTimeout(r, 50));
    first.unmount();
    tokenStore.clear();
    renderDoc();
    await waitFor(() => expect(seen).toHaveLength(2));
    expect(seen).toEqual([null, '8']);
  });
});

// U31: each run control and step action walked through on the page against
// a simulated server that changes state the way the real one does, so a
// control that sends the wrong action, or a page that never shows the
// result, fails here and not only in the browser suite.
describe('run controls, rerun, report and signals on the page (U31)', () => {
  const ADMIN = ['executions:admin'];
  const RERUN = 'exe_01JZZZZZZZZZZZZZZZZZZZZZZZ';

  it('Pause, then Resume, then Cancel: each sends its action and the page follows the run', async () => {
    let status: Doc['header']['status'] = 'running';
    const patches: unknown[] = [];
    server.use(
      ...baseHandlers(
        () =>
          runningDoc((d) => {
            d.header.status = status;
            if (status === 'cancelled') d.header.phase = 'completed';
          }),
        { extraScopes: ADMIN },
      ),
      http.patch('*/v1/executions/:exe', async ({ request }) => {
        const body = (await request.json()) as { action: 'pause' | 'resume' | 'cancel' };
        patches.push(body);
        status = ({ pause: 'paused', resume: 'running', cancel: 'cancelled' } as const)[
          body.action
        ];
        return HttpResponse.json({});
      }),
    );
    renderDoc();
    const user = userEvent.setup();

    await user.click(await screen.findByRole('button', { name: /^pause$/i }));
    expect(await screen.findByRole('button', { name: /^resume$/i })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^pause$/i })).not.toBeInTheDocument();
    expect(patches).toEqual([{ action: 'pause' }]);

    await user.click(screen.getByRole('button', { name: /^resume$/i }));
    expect(await screen.findByRole('button', { name: /^pause$/i })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^resume$/i })).not.toBeInTheDocument();
    expect(patches).toEqual([{ action: 'pause' }, { action: 'resume' }]);

    // Cancel asks first: dismissing the dialog sends nothing.
    await user.click(screen.getByRole('button', { name: /^cancel run$/i }));
    await user.click(
      within(await screen.findByRole('alertdialog')).getByRole('button', { name: /^cancel$/i }),
    );
    expect(patches).toHaveLength(2);
    await user.click(screen.getByRole('button', { name: /^cancel run$/i }));
    const dialog = await screen.findByRole('alertdialog');
    await user.click(within(dialog).getByRole('button', { name: /^cancel run$/i }));
    await waitFor(() =>
      expect(screen.queryByRole('button', { name: /^cancel run$/i })).not.toBeInTheDocument(),
    );
    expect(patches).toEqual([{ action: 'pause' }, { action: 'resume' }, { action: 'cancel' }]);
    expect(screen.queryByRole('button', { name: /^pause$/i })).not.toBeInTheDocument();
    expect(screen.getAllByRole('status').map((el) => el.textContent)).toContain('Completed');
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('Rerun starts a new run with one key and opens it', async () => {
    const doc = runningDoc((d) => {
      d.header.status = 'failed';
      d.header.phase = 'completed';
    });
    const requests: { key: string | null; body: unknown }[] = [];
    server.use(
      ...baseHandlers(doc),
      http.post('*/v1/executions/:exe/rerun', async ({ request, params }) => {
        expect(params.exe).toBe(EXE);
        requests.push({
          key: request.headers.get('idempotency-key'),
          body: await request.json(),
        });
        return HttpResponse.json({ ...doc.header, id: RERUN });
      }),
    );
    const { router } = renderDoc();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /^rerun$/i }));
    await waitFor(() => expect(router.state.location.pathname).toBe(`/x/${RERUN}`));
    expect(requests).toHaveLength(1);
    expect(requests[0]?.key).toBeTruthy();
    expect(requests[0]?.body).toEqual({});
  });

  it('the human report form: outputs and a summary go in one report, and the step shows its result', async () => {
    let reported = false;
    const bodies: Record<string, unknown>[] = [];
    server.use(
      ...baseHandlers(() =>
        runningDoc((d) => {
          const section = d.sections[0];
          const step = d.header.steps[0];
          if (reported && section && step) {
            section.status = 'succeeded';
            section.verdict = 'pass';
            step.status = 'succeeded';
            step.claim = null;
          }
        }),
      ),
      http.post('*/v1/step-runs/:sr/claim', () =>
        HttpResponse.json({ id: 'clm_1', generation: 1 }),
      ),
      http.get('*/v1/step-runs/:sr', ({ params }) =>
        HttpResponse.json({
          step_run_id: params.sr,
          instructions: 'Read the error rate from the dashboard.',
          checklist: [],
          parameters: {},
          output_schema: {
            type: 'object',
            properties: {
              error_rate: { type: 'number', title: 'Error rate' },
              region: { type: 'string', title: 'Region' },
            },
            required: ['error_rate'],
          },
          skill: null,
          template: { instructions: 'Read the error rate.', checklist: [], values: {} },
        }),
      ),
      http.post('*/v1/step-runs/:sr/report', async ({ request }) => {
        bodies.push((await request.json()) as Record<string, unknown>);
        reported = true;
        return HttpResponse.json({});
      }),
    );
    renderDoc();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /claim this step/i }));
    await user.type(await screen.findByRole('textbox', { name: /error rate/i }), '0.004');
    await user.type(screen.getByRole('textbox', { name: /region/i }), 'eu-west');
    await user.type(screen.getByRole('textbox', { name: /^summary/i }), 'Below the 1% budget');
    await user.click(screen.getByRole('button', { name: /^done$/i }));
    await waitFor(() => expect(bodies).toHaveLength(1));
    expect(bodies[0]).toEqual(
      expect.objectContaining({
        status: 'succeeded',
        outputs: { error_rate: 0.004, region: 'eu-west' },
        summary: 'Below the 1% budget',
        claim: { id: 'clm_1', generation: 1 },
      }),
    );
    // The refreshed document shows the result; the form and the claim are gone.
    await waitFor(() =>
      expect(screen.queryByRole('button', { name: /^done$/i })).not.toBeInTheDocument(),
    );
    expect(screen.queryByRole('textbox', { name: /error rate/i })).not.toBeInTheDocument();
    expect(tabClaims.all()).toEqual({});
  });

  it('Signals: the waiting step takes a named signal, and the form goes once the wait is over', async () => {
    let signalled = false;
    const sent: { name: unknown; key: string | null; body: unknown }[] = [];
    server.use(
      ...baseHandlers(() =>
        runningDoc((d) => {
          const section = d.sections[0];
          if (section) {
            section.kind = 'wait';
            section.status = signalled ? 'succeeded' : 'waiting';
          }
        }),
      ),
      http.post('*/v1/executions/:exe/signals/:name', async ({ request, params }) => {
        expect(params.exe).toBe(EXE);
        sent.push({
          name: params.name,
          key: request.headers.get('idempotency-key'),
          body: await request.json(),
        });
        signalled = true;
        return HttpResponse.json({});
      }),
    );
    renderDoc();
    const user = userEvent.setup();
    const send = await screen.findByRole('button', { name: /send signal/i });
    // Nothing to send until the signal is named.
    expect(send).toBeDisabled();
    await user.type(screen.getByLabelText('Signal name'), '  deploy-done ');
    await user.click(send);
    await waitFor(() => expect(sent).toHaveLength(1));
    expect(sent[0]).toEqual({
      name: 'deploy-done',
      key: expect.stringMatching(/.+/),
      body: { payload: {} },
    });
    await waitFor(() =>
      expect(screen.queryByRole('button', { name: /send signal/i })).not.toBeInTheDocument(),
    );
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });
});

describe('walkthrough fixes', () => {
  const DANA = 'usr_01M76R2QSVVFRN94EPY96GDTND';
  const STRANGER = 'usr_01M76R2QZ86FKFYJG2JX2PWR4P';
  const directory = () =>
    http.get('*/v1/orgs/:org/users', () =>
      HttpResponse.json({
        items: [{ id: DANA, name: 'Dana Directory', role: 'member', active: true }],
        next_cursor: null,
      }),
    );

  it('#1 shows people by directory name; an unknown id as a short id', async () => {
    const doc = runningDoc((d) => {
      const [first, second] = d.sections;
      if (first) first.actor = { kind: 'human', user_id: STRANGER };
      if (second) {
        second.actor = { kind: 'human', user_id: DANA };
        second.outputs = { decided_by: DANA };
      }
    });
    server.use(...baseHandlers(doc));
    server.use(directory());
    renderDoc();
    expect((await screen.findAllByText('Dana Directory')).length).toBe(2);
    expect(screen.getByText('usr_…PWR4P')).toBeInTheDocument();
    expect(screen.queryByText(DANA)).not.toBeInTheDocument();
    expect(screen.queryByText(STRANGER)).not.toBeInTheDocument();
  });
});

describe('walkthrough fixes: control nodes and the rail', () => {
  /** A switch (ready, as the engine reports it) whose approval child waits. */
  function switchWaitingOnApproval(): Doc {
    const doc = structuredClone(fixtures.executionDocument) as Doc;
    doc.header.status = 'waiting';
    doc.header.project_id = 'prj_1';
    const approval = doc.sections.find((s) => s.approval);
    const first = doc.sections[0];
    if (!approval || !first) throw new Error('fixture changed');
    const container: Doc['sections'][number] = {
      ...structuredClone(first),
      step_run_id: 'sr_01JABCDEFGHJKMNPQRSTVWXYZ9',
      title: 'Keep or roll back',
      path: 'decide',
      kind: 'switch',
      status: 'ready',
      actor: null,
      verdict: null,
      outputs: {},
    };
    doc.sections = [first, container, approval];
    return doc;
  }

  it('#4 shows a switch waiting on its approval as Running, and the approval as current', async () => {
    server.use(...baseHandlers(switchWaitingOnApproval()));
    renderDoc();
    const heading = await screen.findByRole('heading', { name: /keep or roll back/i });
    const section = heading.closest('section') as HTMLElement;
    expect(within(section).getByText('Running')).toBeInTheDocument();
    expect(within(section).queryByText('Ready')).not.toBeInTheDocument();
    const rail = screen.getByRole('navigation', { name: /steps navigation/i });
    const railItem = within(rail).getByRole('button', { name: /keep or roll back/i });
    expect(railItem).toHaveTextContent(/running/);
    expect(railItem).not.toHaveAttribute('aria-current');
    // The step a person can act on is the current one, not its container.
    expect(within(rail).getByRole('button', { name: /signoff/i })).toHaveAttribute(
      'aria-current',
      'step',
    );
  });
});

describe('walkthrough fixes: the desktop rail stays in view (#3)', () => {
  it('pins the rail pane below the top bar instead of letting it scroll away', async () => {
    server.use(...baseHandlers(runningDoc()));
    const { container } = renderDoc();
    await screen.findByRole('navigation', { name: /steps navigation/i });
    const split = container.querySelector('[data-slot="split-pane"]') as HTMLElement;
    const left = container.querySelector('[data-slot="split-pane-left"]') as HTMLElement;
    // overflow-hidden on the frame would make it the sticky pane's scroller.
    expect(split.className).not.toMatch(/(^|\s)overflow-hidden(\s|$)/);
    expect(left.className).toMatch(/md:sticky/);
    expect(left.className).toMatch(/md:top-/);
    expect(left.className).toMatch(/md:self-start/);
    const rail = container.querySelector('[data-slot="step-rail"]') as HTMLElement;
    expect(rail.className).toMatch(/md:max-h-/);
  });
});

describe('walkthrough fixes: one checklist while claimed (#6)', () => {
  it('shows the read-only checklist until this tab claims, then only the form', async () => {
    server.use(
      ...baseHandlers(runningDoc()),
      http.post('*/v1/step-runs/:sr/claim', () =>
        HttpResponse.json({ id: 'clm_1', generation: 1 }),
      ),
      http.get('*/v1/step-runs/:sr', () =>
        HttpResponse.json({
          step_run_id: 'sr_x',
          instructions: 'Log in as the test user.',
          checklist: ['Login page loads'],
          parameters: {},
          output_schema: { type: 'object', properties: {} },
          skill: null,
        }),
      ),
    );
    renderDoc();
    const user = userEvent.setup();
    const claim = await screen.findByRole('button', { name: /claim this step/i });
    // The section and its actions share one wrapper.
    const step = claim.closest('[data-testid^="actions-"]')?.parentElement as HTMLElement;
    expect(step.querySelectorAll('[data-slot="step-checklist"]')).toHaveLength(1);
    await user.click(claim);
    expect(await within(step).findByRole('checkbox', { name: /login page loads/i })).toBeVisible();
    expect(step.querySelectorAll('[data-slot="step-checklist"]')).toHaveLength(0);
    expect(within(step).getAllByText('Login page loads')).toHaveLength(1);
  });
});

describe('walkthrough fixes: a queued run (#8)', () => {
  it('shows "Queued: waiting for a slot" instead of an empty steps panel', async () => {
    const doc = runningDoc((d) => {
      d.header.status = 'queued';
      d.sections = [];
      d.header.steps = [];
    });
    server.use(...baseHandlers(doc));
    renderDoc();
    expect(await screen.findByText('Queued: waiting for a slot')).toBeInTheDocument();
    expect(screen.queryByRole('navigation', { name: /steps navigation/i })).not.toBeInTheDocument();
  });
});

describe('walkthrough fixes: one document read drives the page (#9)', () => {
  it('never says Completed while the header still says Running', async () => {
    let reads = 0;
    const encoder = new TextEncoder();
    // Every document read still says running.
    const doc = () => {
      reads += 1;
      return runningDoc();
    };
    server.use(...baseHandlers(doc));
    server.use(
      // The stream says the run completed before any document read does.
      http.get(
        '*/v1/executions/:exe/events',
        () =>
          new HttpResponse(
            new ReadableStream<Uint8Array>({
              start(controller) {
                controller.enqueue(
                  encoder.encode('id: 9\nevent: ExecutionCompleted\ndata: {}\n\n'),
                );
                controller.close();
              },
            }),
            { headers: { 'Content-Type': 'text/event-stream' } },
          ),
      ),
    );
    renderDoc();
    const header = () => document.querySelector('[data-slot="run-header"]');
    const statusTexts = () => screen.getAllByRole('status').map((el) => el.textContent);
    await waitFor(() => expect(header()).not.toBeNull());
    // The terminal event refetched the document, which still reads running.
    await waitFor(() => expect(reads).toBeGreaterThanOrEqual(2));
    await waitFor(() => expect(statusTexts()).not.toContain('Connecting…'));
    expect(header()).toHaveTextContent('Running');
    expect(statusTexts()).not.toContain('Completed');
  });
});

describe('walkthrough fixes: the My tasks count stays current (#10)', () => {
  const task = { kind: 'approval', step_run_id: 'sr_x', execution_id: EXE, title: 'Roll back?' };
  const myTasksCount = () =>
    within(screen.getByRole('navigation', { name: 'Primary' }))
      .getByRole('link', { name: /inbox/i })
      .querySelector('[data-slot="sidebar-nav-count"]')?.textContent;

  it('re-reads the inbox after this person decides', async () => {
    let decided = false;
    const doc = runningDoc((d) => {
      const section = d.sections[0];
      if (section) {
        section.status = decided ? 'succeeded' : 'waiting';
        section.approval = {
          schema_revision: 1,
          step_run_id: section.step_run_id,
          status: decided ? 'succeeded' : 'waiting',
          prompt: 'Roll back or keep?',
          options: [{ id: 'keep', title: 'Keep deployment', auto: false }],
          eligible_recipients: [{ kind: 'user', reference: 'usr_1' }],
          escalation_recipients: [],
          escalated: false,
          quorum: 1,
          decisions: [],
        };
      }
    });
    server.use(...baseHandlers(() => doc));
    server.use(
      http.get('*/v1/me/tasks', () =>
        HttpResponse.json({ items: decided ? [] : [task], next_cursor: null }),
      ),
      http.post('*/v1/step-runs/:sr/decisions', () => {
        decided = true;
        return HttpResponse.json({});
      }),
    );
    renderDoc();
    const user = userEvent.setup();
    await waitFor(() => expect(myTasksCount()).toBe('1'));
    const actions = await screen.findByTestId(/actions-/);
    await vote(user, actions, /keep deployment/i);
    await waitFor(() => expect(myTasksCount()).toBe('0'));
  });

  it('re-reads the inbox when a live event arrives for the run on screen', async () => {
    let assigned = false;
    let sent = false;
    const encoder = new TextEncoder();
    server.use(...baseHandlers(runningDoc()));
    server.use(
      http.get('*/v1/me/tasks', () =>
        HttpResponse.json({ items: assigned ? [task] : [], next_cursor: null }),
      ),
      http.get(
        '*/v1/executions/:exe/events',
        () =>
          new HttpResponse(
            new ReadableStream<Uint8Array>({
              start(controller) {
                if (sent) return;
                sent = true;
                // Someone else's step finished and an approval opened for this person.
                setTimeout(() => {
                  assigned = true;
                  controller.enqueue(encoder.encode('id: 5\nevent: StepReady\ndata: {}\n\n'));
                }, 200);
              },
            }),
            { headers: { 'Content-Type': 'text/event-stream' } },
          ),
      ),
    );
    renderDoc();
    await waitFor(() => expect(myTasksCount()).toBe('0'));
    await waitFor(() => expect(myTasksCount()).toBe('1'));
  });
});

describe('take over an agent step (LANG7)', () => {
  it('offers "Take over" on a ready agent step and claims with take_over', async () => {
    let body: unknown;
    const doc = runningDoc((d) => {
      const section = d.sections[0];
      if (section) section.task_actor = 'agent';
    });
    server.use(
      // The human brief never lists agent work (ahead of the base brief,
      // which lists the first step).
      http.get('*/v1/executions/:exe/next', () => HttpResponse.json(briefFor([]))),
      ...baseHandlers(doc),
      http.post('*/v1/step-runs/:sr/claim', async ({ request }) => {
        body = await request.json();
        return HttpResponse.json({ id: 'clm_1', generation: 1 });
      }),
      http.get('*/v1/step-runs/:sr', () =>
        HttpResponse.json({
          step_run_id: 'sr_x',
          instructions: 'Deploy by hand.',
          checklist: [],
          parameters: {},
          output_schema: { type: 'object', properties: {} },
          skill: null,
        }),
      ),
    );
    renderDoc();
    const user = userEvent.setup();
    expect(screen.queryByRole('button', { name: /claim this step/i })).not.toBeInTheDocument();
    await user.click(await screen.findByRole('button', { name: /take over/i }));
    expect(await screen.findByText(/deploy by hand/i)).toBeInTheDocument();
    expect(body).toMatchObject({ actor: { kind: 'human' }, take_over: true });
  });
});

// LANG7 review round 1: no action the server would refuse.
describe('strict and agent steps offer only what the server allows (LANG7 review)', () => {
  it('offers a plain claim, not "Take over", on a handed-off agent step', async () => {
    const doc = runningDoc((d) => {
      const section = d.sections[0];
      if (section) section.task_actor = 'agent';
    });
    // The human brief lists the handed-off step as a person's work.
    server.use(...baseHandlers(doc));
    renderDoc();
    expect(await screen.findByRole('button', { name: 'Claim this step' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /take over/i })).not.toBeInTheDocument();
  });

  it('offers no "Take over" while the run is paused', async () => {
    const doc = runningDoc((d) => {
      d.header.status = 'paused';
      const section = d.sections[0];
      if (section) section.task_actor = 'agent';
    });
    server.use(
      http.get('*/v1/executions/:exe/next', () => HttpResponse.json(briefFor([]))),
      ...baseHandlers(doc),
    );
    renderDoc();
    expect((await screen.findAllByText('Health endpoint returns 200')).length).toBeGreaterThan(0);
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(screen.queryByRole('button', { name: /take over/i })).not.toBeInTheDocument();
  });

  it('shows a strict assignee instead of a claim to anyone else', async () => {
    const doc = runningDoc((d) => {
      const section = d.sections[0];
      if (!section) return;
      section.task_actor = 'human';
      section.assignee = {
        to: 'user:bob',
        strict: true,
        recipients: [{ kind: 'user', reference: 'usr_2' }],
      };
    });
    server.use(...baseHandlers(doc));
    renderDoc();
    expect(await screen.findByText(/assigned to .* \(strict\)/i)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Claim this step' })).not.toBeInTheDocument();
  });

  it('offers the claim to the strict assignee', async () => {
    const doc = runningDoc((d) => {
      const section = d.sections[0];
      if (!section) return;
      section.task_actor = 'any';
      section.assignee = {
        to: 'team:oncall',
        strict: true,
        recipients: [{ kind: 'team', reference: 'team_1' }],
      };
    });
    server.use(...baseHandlers(doc));
    renderDoc();
    expect(await screen.findByRole('button', { name: 'Claim this step' })).toBeInTheDocument();
    expect(screen.queryByText(/assigned to .* \(strict\)/i)).not.toBeInTheDocument();
  });
});
