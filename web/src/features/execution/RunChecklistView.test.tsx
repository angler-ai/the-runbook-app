// ONB-VIEW: tests for the Checklist view of a run (?view=checklist).
// Tests progress counts, next-up waiting labels, parallel "Any order",
// switch taken branch, inline actions gating, live updates, URL query, and axe.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
  useNavigate,
  useParams,
  useSearch,
} from '@tanstack/react-router';
import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it } from 'vitest';
import type { Schemas } from '../../api/client';
import { tokenStore } from '../../api/token-store';
import { fixtures } from '../../test/msw/fixtures';
import { server } from '../../test/msw/server';
import { expectNoAxeViolations } from '../../test/axe';
import type { Outline } from '../../ui/types';
import { OrgProvider } from '../context/OrgContext';
import type { CompilerBackend } from '../compile/useCompiler';
import { ExecutionDocumentPage } from './ExecutionDocumentPage';
import { forgetLiveResumePoints } from './live';
import { validateRunSearch, type RunSearch } from './views';

type Doc = Schemas['ExecutionDocumentView'];
type Section = Schemas['StepSection'];
type Step = Schemas['StepView'];

const baseSection = fixtures.executionDocument.sections[0]!;
const baseStep = fixtures.executionDocument.header.steps[0]!;
const baseHeader = fixtures.executionDocument.header;

const RUN_ID = 'exe_ONBOARDING000000000001';
const RIVER = 'usr_01JRIVERCHEN000000000001';
const PRIYA = 'usr_01JPRIYANATARAJAN000001';

const me = {
  user: { id: RIVER, name: 'Riley Chen', email: 'riley@example.test' },
  org: { id: 'org_1', title: 'Acme' },
  credential: {
    kind: 'pat',
    scopes: ['runbooks:read', 'executions:read', 'executions:act', 'executions:start'],
  },
  teams: [
    { id: 'team_customer-success', slug: 'customer-success', title: 'Customer Success' },
    { id: 'team_solutions', slug: 'solutions', title: 'Solutions' },
  ],
};

const onboardingOutline: Outline = {
  id: 'customer-onboarding',
  title: 'Customer onboarding',
  stepBudget: null,
  steps: [
    {
      id: 'kickoff',
      title: 'Kickoff call with the customer',
      path: '/spec/steps/0',
      kind: 'task',
      actor: 'human',
      assignee: 'team:customer-success',
      instructions: 'Run the kickoff call.',
      checklist: ['Goals and success criteria agreed', 'Technical contact named', 'Go-live date proposed'],
    },
    {
      id: 'provision',
      title: 'Provision the workspace',
      path: '/spec/steps/1',
      kind: 'task',
      actor: 'agent',
      instructions: 'Create the workspace.',
    },
    {
      id: 'setup',
      title: 'Set up the account',
      path: '/spec/steps/2',
      kind: 'parallel',
      children: [
        {
          id: 'sso',
          title: 'Configure SSO with the customer’s identity provider',
          path: '/spec/steps/2/parallel/0',
          kind: 'task',
          actor: 'human',
          assignee: 'team:solutions',
        },
        {
          id: 'import_history',
          title: 'Import historical data',
          path: '/spec/steps/2/parallel/1',
          kind: 'task',
          actor: 'agent',
        },
        {
          id: 'billing',
          title: 'Set up billing and invoicing',
          path: '/spec/steps/2/parallel/2',
          kind: 'task',
          actor: 'human',
          assignee: 'user:Priya Natarajan',
        },
        {
          id: 'customer_connects',
          title: 'Customer connects their store',
          path: '/spec/steps/2/parallel/3',
          kind: 'wait',
          wait: {
            kind: 'signal',
            signal: 'store_connected',
          },
        },
      ],
    },
    {
      id: 'first_events',
      title: 'First events received',
      path: '/spec/steps/3',
      kind: 'task',
      check: true,
    },
    {
      id: 'go_live',
      title: 'Approve go-live',
      path: '/spec/steps/4',
      kind: 'approval',
      approval: {
        prompt: 'Everything is set up. Switch them to live?',
        options: [
          { id: 'go_live', title: 'go_live' },
          { id: 'hold', title: 'hold' },
        ],
        approvers: ['team:customer-success'],
        min: 1,
      },
    },
    {
      id: 'welcome',
      title: 'Send the welcome pack',
      path: '/spec/steps/5',
      kind: 'task',
    },
  ],
  finally: [],
};

const switchOutline: Outline = {
  id: 'customer-onboarding-switch',
  title: 'Customer onboarding',
  stepBudget: null,
  steps: [
    {
      id: 'kickoff',
      title: 'Kickoff call with the customer',
      path: '/spec/steps/0',
      kind: 'task',
      actor: 'human',
    },
    {
      id: 'tier_check',
      title: 'Plan branch',
      path: '/spec/steps/1',
      kind: 'switch',
      branches: [
        {
          kind: 'case',
          label: 'enterprise',
          when: "${{ inputs.plan == 'enterprise' }}",
          children: [
            {
              id: 'enterprise_vip',
              title: 'Enterprise VIP onboarding review',
              path: '/spec/steps/1/switch/cases/0/steps/0',
              kind: 'task',
            },
          ],
        },
        {
          kind: 'default',
          label: 'default',
          children: [
            {
              id: 'standard_welcome',
              title: 'Standard automated welcome',
              path: '/spec/steps/1/switch/default/steps/0',
              kind: 'task',
            },
          ],
        },
      ],
    },
  ],
  finally: [],
};

afterEach(() => {
  forgetLiveResumePoints();
  tokenStore.clear();
  sessionStorage.clear();
});

function controllableStream() {
  const encoder = new TextEncoder();
  let push: (text: string) => void = () => {};
  const stream = () =>
    new ReadableStream<Uint8Array>({
      start(controller) {
        push = (text) => controller.enqueue(encoder.encode(text));
        controller.enqueue(encoder.encode(': keep-alive\n\n'));
      },
    });
  return {
    stream,
    send: (id: number, event: string) => push(`id: ${id}\nevent: ${event}\ndata: {}\n\n`),
  };
}

interface RenderOptions {
  outline?: Outline;
  stream?: () => ReadableStream<Uint8Array>;
  onClaim?: (body: unknown) => void;
}

function renderRun(path: string, doc: () => Doc, options: RenderOptions = {}) {
  const currentOutline = options.outline ?? onboardingOutline;
  const compiler: CompilerBackend = {
    validate: async () => ({ diagnostics: [], step_budget: 1 }),
    outline: async () => ({ outline: currentOutline, diagnostics: [] }),
    describe: async () => ({ runbook: null, diagnostics: [] }),
  };

  server.use(
    http.get('*/v1/me', () => HttpResponse.json(me)),
    http.get('*/v1/meta', () =>
      HttpResponse.json({
        edition: 'self_hosted',
        version: '0.0.0',
        auth: { method: 'local' },
        features: { signup: true },
      }),
    ),
    http.get('*/v1/orgs', () =>
      HttpResponse.json({ items: [{ id: 'org_1', title: 'Acme' }], next_cursor: null }),
    ),
    http.get('*/v1/projects/:p/members', () =>
      HttpResponse.json({
        items: [{ user_id: RIVER, role: 'operator' }],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/orgs/:org/users', () =>
      HttpResponse.json({
        items: [
          { id: RIVER, name: 'Riley Chen', email: 'riley@example.test', role: 'member' },
          { id: PRIYA, name: 'Priya Natarajan', email: 'priya@example.test', role: 'member' },
        ],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/orgs/:org/teams', () =>
      HttpResponse.json({
        items: [
          { id: 'team_customer-success', slug: 'customer-success', title: 'Customer Success' },
          { id: 'team_solutions', slug: 'solutions', title: 'Solutions' },
        ],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/orgs/:org/projects', () =>
      HttpResponse.json({ items: [{ id: 'prj_1', slug: 'default', title: 'Default' }], next_cursor: null }),
    ),
    http.get('*/v1/projects/:p/runbooks', () =>
      HttpResponse.json({ items: [], next_cursor: null }),
    ),
    http.get('*/v1/projects/:p/executions', () =>
      HttpResponse.json({ items: [], next_cursor: null }),
    ),
    http.get('*/v1/me/tasks', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/runbooks/:rb', ({ params }) =>
      HttpResponse.json({
        id: params.rb,
        slug: 'customer-onboarding',
        title: 'Customer onboarding',
        latest_version: 1,
      }),
    ),
    http.get('*/v1/runbooks/:rb/versions/:version', () =>
      HttpResponse.json({ source: 'runbook: test\n' }),
    ),
    http.get('*/v1/executions/:exe/document', () => HttpResponse.json(doc())),
    http.get('*/v1/executions/:exe', ({ params }) =>
      HttpResponse.json({ ...doc().header, id: params.exe }),
    ),
    http.get('*/v1/executions/:exe/report', () => HttpResponse.json(doc())),
    http.get('*/v1/executions/:exe/next', () => {
      const d = doc();
      const readyWork = d.sections
        .filter((s) => s.status === 'ready' || s.status === 'claimed')
        .map((s) => ({
          step_run_id: s.step_run_id,
          path: s.path,
          title: s.title,
          actor_kind: s.actor?.kind === 'agent' ? 'agent' : 'human',
        }));
      return HttpResponse.json({
        schema_revision: 1,
        execution_id: RUN_ID,
        completed_steps: [],
        decisions: [],
        comments: [],
        ready_work: readyWork,
        next_action: readyWork.length > 0 ? 'claim_ready_work' : 'wait',
      });
    }),
    http.get('*/v1/step-runs/:sr/logs', () =>
      HttpResponse.json({ lines: [], has_more: false, next_cursor: '0' }),
    ),
    http.get('*/v1/step-runs/:sr', ({ params }) =>
      HttpResponse.json({
        step_run_id: params.sr,
        checklist: [],
        output_schema: null,
      }),
    ),
    http.post('*/v1/step-runs/:sr/claim', async ({ request }) => {
      options.onClaim?.(await request.json());
      return HttpResponse.json({ id: 'clm_1', generation: 1 });
    }),
    http.get(
      '*/v1/executions/:exe/events',
      () =>
        new HttpResponse(options.stream ? options.stream() : new ReadableStream({ start() {} }), {
          headers: { 'Content-Type': 'text/event-stream' },
        }),
    ),
  );

  tokenStore.set('trb_pat_good');

  const root = createRootRoute({
    component: () => (
      <main>
        <Outlet />
      </main>
    ),
  });

  function Page() {
    const { exe } = useParams({ strict: false }) as { exe: string };
    const search = useSearch({ strict: false }) as RunSearch;
    const navigate = useNavigate();
    return (
      <ExecutionDocumentPage
        executionId={exe}
        view={search.view ?? 'graph'}
        onViewChange={(view) =>
          void navigate({ to: '.', search: view === 'graph' ? {} : { view }, replace: true })
        }
        compiler={compiler}
      />
    );
  }

  const page = createRoute({
    getParentRoute: () => root,
    path: '/x/$exe',
    validateSearch: validateRunSearch,
    component: Page,
  });

  const router = createRouter({
    routeTree: root.addChildren([page]),
    history: createMemoryHistory({ initialEntries: [path] }),
  });

  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const utils = render(
    <QueryClientProvider client={client}>
      <OrgProvider>
        <RouterProvider router={router} />
      </OrgProvider>
    </QueryClientProvider>,
  );

  return { router, client, ...utils };
}

function buildOnboardingDoc(customizer?: (sections: Section[], steps: Step[]) => void): Doc {
  const sectionsData: Partial<Section>[] = [
    {
      step_run_id: 'sr_kickoff',
      path: 'kickoff',
      title: 'Kickoff call with the customer',
      kind: 'task',
      status: 'succeeded',
      completed_at_ms: 1_794_500_010_000,
      actor: { kind: 'human', user_id: RIVER },
    },
    {
      step_run_id: 'sr_provision',
      path: 'provision',
      title: 'Provision the workspace',
      kind: 'task',
      status: 'succeeded',
      completed_at_ms: 1_794_500_040_000,
      actor: { kind: 'agent', name: 'release-agent', principal_id: 'p_1', session_id: 's_1' },
    },
    {
      step_run_id: 'sr_setup',
      path: 'setup',
      title: 'Set up the account',
      kind: 'parallel',
      status: 'ready',
    },
    {
      step_run_id: 'sr_sso',
      path: 'setup.sso',
      title: 'Configure SSO with the customer’s identity provider',
      kind: 'task',
      status: 'succeeded',
      completed_at_ms: 1_794_500_070_000,
      actor: { kind: 'human', user_id: RIVER },
    },
    {
      step_run_id: 'sr_import',
      path: 'setup.import_history',
      title: 'Import historical data',
      kind: 'task',
      status: 'succeeded',
      completed_at_ms: 1_794_500_100_000,
      actor: { kind: 'agent', name: 'release-agent', principal_id: 'p_1', session_id: 's_1' },
    },
    {
      step_run_id: 'sr_billing',
      path: 'setup.billing',
      title: 'Set up billing and invoicing',
      kind: 'task',
      status: 'ready',
      assignee: { to: 'user:Priya Natarajan', strict: true, recipients: [{ kind: 'user', reference: PRIYA }] },
    },
    {
      step_run_id: 'sr_customer_connects',
      path: 'setup.customer_connects',
      title: 'Customer connects their store',
      kind: 'wait',
      status: 'waiting',
    },
    {
      step_run_id: 'sr_first_events',
      path: 'first_events',
      title: 'First events received',
      kind: 'task',
      status: 'pending',
    },
    {
      step_run_id: 'sr_go_live',
      path: 'go_live',
      title: 'Approve go-live',
      kind: 'approval',
      status: 'waiting',
      approval: {
        step_run_id: 'sr_go_live',
        prompt: 'Everything is set up. Switch them to live?',
        options: [
          { id: 'go_live', title: 'go_live', auto: false },
          { id: 'hold', title: 'hold', auto: false },
        ],
        decisions: [],
        eligible_recipients: [{ kind: 'team', reference: 'team_customer-success' }],
        escalated: false,
        escalation_recipients: [],
        quorum: 1,
        schema_revision: 1,
        status: 'waiting',
      },
    },
    {
      step_run_id: 'sr_welcome',
      path: 'welcome',
      title: 'Send the welcome pack',
      kind: 'task',
      status: 'skipped',
    },
  ];

  const sectionsWithDefaults: Section[] = sectionsData.map((s) => ({
    ...baseSection,
    actor: null,
    assignee: null,
    approval: null,
    completed_at_ms: null,
    started_at_ms: null,
    claimed_at_ms: null,
    outputs: {},
    evidence: [],
    ...s,
  }));

  const steps: Step[] = sectionsWithDefaults.map((s) => ({
    ...baseStep,
    id: s.step_run_id,
    path: s.path,
    kind: s.kind,
    status: s.status,
    attempt: 1,
    eligible_recipients: s.approval?.eligible_recipients ?? [],
    assignees: s.assignee?.recipients ?? [],
    wait: s.kind === 'wait' ? { signal: 'store_connected' } : undefined,
  }));

  customizer?.(sectionsWithDefaults, steps);

  return {
    header: {
      ...baseHeader,
      id: RUN_ID,
      title: 'Onboard Acme Corp',
      status: 'running',
      phase: 'ordinary',
      project_id: 'prj_1',
      runbook_id: 'rb_onboarding',
      runbook_slug: 'customer-onboarding',
      runbook_version: 1,
      started_at_ms: 1_794_500_000_000,
      steps,
      inputs: { customer: 'Acme Corp', plan: 'enterprise' },
      fields: {},
      notifications_not_delivered: [],
    },
    sections: sectionsWithDefaults,
  };
}

describe('Checklist view (ONB-VIEW)', () => {
  it('shows progress counts at the top, including skipped steps', async () => {
    const doc = buildOnboardingDoc();
    renderRun(`/x/${RUN_ID}?view=checklist`, () => doc);

    // 4 of 9 done: kickoff, provision, sso, import_history = 4 succeeded out of 9 total leaf steps
    expect(await screen.findByText('4 of 9 done')).toBeInTheDocument();
    const progressbar = screen.getByRole('progressbar', { name: '4 of 9 done' });
    expect(progressbar).toHaveAttribute('aria-valuenow', '4');
    expect(progressbar).toHaveAttribute('aria-valuemax', '9');

    // 1 skipped step (welcome)
    expect(screen.getByText('1 skipped')).toBeInTheDocument();
  });

  it('shows next-up "waiting on" labels for a person, an agent, a signal and an approval', async () => {
    const doc = buildOnboardingDoc((sections, steps) => {
      // Make provision ready to test the "Any agent" label
      const provSec = sections.find((s) => s.path === 'provision')!;
      const provStep = steps.find((s) => s.path === 'provision')!;
      provSec.status = 'ready';
      provSec.actor = undefined;
      provStep.status = 'ready';
    });

    renderRun(`/x/${RUN_ID}?view=checklist`, () => doc);

    const nextUp = await screen.findByRole('region', { name: 'Next up' });
    expect(nextUp).toBeInTheDocument();

    // 1. Agent task unclaimed: "Any agent"
    expect(within(nextUp).getByText('Provision the workspace')).toBeInTheDocument();
    expect(within(nextUp).getByText('Any agent')).toBeInTheDocument();

    // 2. Person: "Priya Natarajan"
    expect(within(nextUp).getByText('Set up billing and invoicing')).toBeInTheDocument();
    expect(await within(nextUp).findByText('Priya Natarajan')).toBeInTheDocument();

    // 3. Signal: "Waiting for store_connected"
    expect(within(nextUp).getByText('Customer connects their store')).toBeInTheDocument();
    expect(within(nextUp).getByText('Waiting for store_connected')).toBeInTheDocument();

    // 4. Approval: "Needs approval from team customer-success" (or team Customer Success)
    expect(within(nextUp).getByText('Approve go-live')).toBeInTheDocument();
    expect(await within(nextUp).findByText(/Needs approval from/i)).toBeInTheDocument();
  });

  it('marks a parallel section with "Any order"', async () => {
    const doc = buildOnboardingDoc();
    renderRun(`/x/${RUN_ID}?view=checklist`, () => doc);

    await screen.findByText('Set up the account');
    const badge = screen.getByText('Any order');
    expect(badge).toBeInTheDocument();
  });

  it('shows only the taken branch for a switch', async () => {
    const switchSteps: Step[] = [
      { ...baseStep, id: 'sr_kickoff', path: 'kickoff', kind: 'task', status: 'succeeded', attempt: 1 },
      { ...baseStep, id: 'sr_tier_check', path: 'tier_check', kind: 'switch', status: 'ready', attempt: 1, branch: { kind: 'case', index: 0 } },
      { ...baseStep, id: 'sr_vip', path: 'tier_check.enterprise_vip', kind: 'task', status: 'ready', attempt: 1 },
      { ...baseStep, id: 'sr_std', path: 'tier_check.standard_welcome', kind: 'task', status: 'pending', attempt: 1 },
    ];
    const switchSections: Section[] = [
      {
        ...baseSection,
        step_run_id: 'sr_kickoff',
        path: 'kickoff',
        title: 'Kickoff call with the customer',
        kind: 'task',
        status: 'succeeded',
        outputs: {},
        evidence: [],
      },
      {
        ...baseSection,
        step_run_id: 'sr_tier_check',
        path: 'tier_check',
        title: 'Plan branch',
        kind: 'switch',
        status: 'ready',
        outputs: {},
        evidence: [],
      },
      {
        ...baseSection,
        step_run_id: 'sr_vip',
        path: 'tier_check.enterprise_vip',
        title: 'Enterprise VIP onboarding review',
        kind: 'task',
        status: 'ready',
        outputs: {},
        evidence: [],
      },
      {
        ...baseSection,
        step_run_id: 'sr_std',
        path: 'tier_check.standard_welcome',
        title: 'Standard automated welcome',
        kind: 'task',
        status: 'pending',
        outputs: {},
        evidence: [],
      },
    ];
    const switchDoc: Doc = {
      header: {
        ...baseHeader,
        id: RUN_ID,
        title: 'Onboard Acme Corp',
        status: 'running',
        phase: 'ordinary',
        project_id: 'prj_1',
        runbook_id: 'rb_switch',
        runbook_slug: 'customer-onboarding-switch',
        runbook_version: 1,
        started_at_ms: 1_794_500_000_000,
        steps: switchSteps,
        inputs: { plan: 'enterprise' },
        fields: {},
        notifications_not_delivered: [],
      },
      sections: switchSections,
    };

    renderRun(`/x/${RUN_ID}?view=checklist`, () => switchDoc, { outline: switchOutline });

    // The enterprise branch is taken
    const vipElements = await screen.findAllByText('Enterprise VIP onboarding review');
    expect(vipElements.length).toBeGreaterThan(0);
    // The default branch was not taken and should not appear
    expect(screen.queryByText('Standard automated welcome')).not.toBeInTheDocument();
  });

  it('shows actions only where allowed', async () => {
    let claimed = false;
    const doc = buildOnboardingDoc((sections, steps) => {
      // Make sso ready; Riley Chen is in team_solutions so they can claim it!
      const ssoSec = sections.find((s) => s.path === 'setup.sso')!;
      const ssoStep = steps.find((s) => s.path === 'setup.sso')!;
      ssoSec.status = 'ready';
      ssoSec.assignee = { to: 'team:solutions', strict: false };
      ssoStep.status = 'ready';
    });

    renderRun(`/x/${RUN_ID}?view=checklist`, () => doc, {
      onClaim: () => {
        claimed = true;
      },
    });

    // Riley Chen can claim sso because they are in team:solutions
    const claimButtons = await screen.findAllByRole('button', { name: /Claim this step/i });
    expect(claimButtons.length).toBeGreaterThan(0);

    // Clicking claim invokes onClaim handler
    await userEvent.click(claimButtons[0]!);
    expect(claimed).toBe(true);

    // Completed steps (e.g. kickoff) do not show claim buttons
    const kickoffHeading = screen.getAllByText('Kickoff call with the customer')[0]!;
    const kickoffSection = kickoffHeading.closest<HTMLElement>('[data-slot="checklist-row"]')!;
    expect(within(kickoffSection).queryByRole('button', { name: /Claim this step/i })).not.toBeInTheDocument();

    // Billing is strictly assigned to Priya Natarajan: Riley Chen is NOT Priya, so no claim button
    const billingRows = screen
      .getAllByText('Set up billing and invoicing')
      .map((el) => el.closest('[data-slot="checklist-row"], [data-slot="next-up-item"]'))
      .filter(Boolean) as HTMLElement[];
    expect(billingRows.length).toBeGreaterThan(0);
    for (const row of billingRows) {
      expect(within(row).queryByRole('button', { name: /Claim this step/i })).not.toBeInTheDocument();
    }
  });

  it('moves a step from next-up to done on a live update', async () => {
    let doc = buildOnboardingDoc((sections, steps) => {
      // Provision is ready initially
      const provSec = sections.find((s) => s.path === 'provision')!;
      const provStep = steps.find((s) => s.path === 'provision')!;
      provSec.status = 'ready';
      provStep.status = 'ready';
    });

    const live = controllableStream();
    renderRun(`/x/${RUN_ID}?view=checklist`, () => doc, { stream: live.stream });

    // Initially provision is in Next up
    const nextUpInitial = await screen.findByRole('region', { name: 'Next up' });
    expect(within(nextUpInitial).getByText('Provision the workspace')).toBeInTheDocument();

    // A live event signals provision completed
    doc = buildOnboardingDoc((sections, steps) => {
      const provSec = sections.find((s) => s.path === 'provision')!;
      const provStep = steps.find((s) => s.path === 'provision')!;
      provSec.status = 'succeeded';
      provSec.actor = { kind: 'agent', name: 'release-agent', principal_id: 'p_1', session_id: 's_1' };
      provSec.completed_at_ms = 1_794_500_050_000;
      provStep.status = 'succeeded';
    });

    act(() => live.send(10, 'StepCompleted'));

    // After the live update, provision is no longer in Next up
    await waitFor(() => {
      const nextUpUpdated = screen.queryByRole('region', { name: 'Next up' });
      if (nextUpUpdated) {
        expect(within(nextUpUpdated).queryByText('Provision the workspace')).not.toBeInTheDocument();
      }
    });

    // In the sections, provision is shown as done by release-agent
    const provRow = screen.getByText('Provision the workspace').closest<HTMLElement>('[data-slot="checklist-row"]')!;
    expect(within(provRow).getByText('release-agent')).toBeInTheDocument();
  });

  it('keeps ?view=checklist in the URL and switches views', async () => {
    const doc = buildOnboardingDoc();
    const { router } = renderRun(`/x/${RUN_ID}?view=checklist`, () => doc);

    expect(await screen.findByRole('article', { name: /as a checklist/i })).toBeInTheDocument();
    expect(router.state.location.search).toEqual({ view: 'checklist' });

    // Switch to Document view
    const docTab = screen.getByRole('tab', { name: 'Document' });
    await userEvent.click(docTab);

    await waitFor(() => {
      expect(router.state.location.search).toEqual({ view: 'document' });
    });

    // Switch back to Checklist view
    const checklistTab = screen.getByRole('tab', { name: 'Checklist' });
    await userEvent.click(checklistTab);

    await waitFor(() => {
      expect(router.state.location.search).toEqual({ view: 'checklist' });
    });
  });

  it('passes axe accessibility checks', async () => {
    const doc = buildOnboardingDoc();
    const { container } = renderRun(`/x/${RUN_ID}?view=checklist`, () => doc);

    await screen.findByRole('article', { name: /as a checklist/i });
    await expectNoAxeViolations(container);
  });
});
