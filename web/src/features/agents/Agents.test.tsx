import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { Schemas } from '../../api/client';
import { AgentAudit } from './AgentsPage';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, test, vi } from 'vitest';
import { http, HttpResponse } from 'msw';
import { server } from '../../test/msw/server';
import type { Credential } from '../runner/Credentials';
import type { Connection } from '../runner/resources';
import { AgentConfiguration } from './AgentConfiguration';
import { Bindings } from './AgentBindings';
import { ProposalList } from './Proposals';
import { TemplateForm } from './Templates';
import { Activity, decisionRows } from './AgentActivity';
import { Versions } from './AgentVersions';

afterEach(cleanup);

test('editor_sees_read_only_credentials_and_tools', () => {
  const row = { id: 'tool', cells: ['Search', 'read'] };
  const view = render(<Bindings kind="tools" rows={[row]} editable={false} initial={[]} />);
  expect(screen.getByRole('table', { name: 'Allowed tools' })).toBeVisible();
  expect(screen.queryByRole('button', { name: 'Save tools' })).toBeNull();
  view.rerender(
    <Bindings
      kind="credentials"
      rows={[{ id: 'abc', cells: ['Read-only key', 'proxy', 'api.example.com'] }]}
      editable={false}
      initial={[]}
    />,
  );
  expect(screen.getByRole('table', { name: 'Allowed credentials' })).toHaveTextContent(
    'Read-only key',
  );
  expect(screen.queryByRole('button', { name: 'Save credentials' })).toBeNull();
});

test('explains the credential requirement on the egress allow-list', () => {
  render(
    <AgentConfiguration
      agent={{ id: 'agent', name: 'checker', description: '', status: 'active', group_id: 'g' }}
      canEdit
      admin
      groups={[{ id: 'g', name: 'Default' }]}
      profiles={[]}
      save={async () => {}}
      operate={async () => {}}
    />,
  );
  expect(screen.getByLabelText('Set an explicit host allow-list')).toHaveAccessibleDescription(
    /exactly one released proxy credential/i,
  );
});

test('publishes checked tools only after their named credential grant is selected', async () => {
  const save = vi.fn(async () => {});
  const credential: Credential = {
    id: 'oauth_key',
    name: 'Workspace credential',
    type: 'mcp-oauth',
    groups: 'all',
    status: 'active',
    label: 'read',
    sharing: { projects: 'all' },
    delivery: ['mcp'],
    config: {},
    secret_meta: {},
  };
  const connection: Connection = {
    id: 'workspace',
    name: 'Workspace',
    url: 'https://workspace.example.test/mcp',
    transport: 'streamable-http',
    credential_id: credential.id,
    sharing: { projects: 'all' },
    pinned_snapshot_id: 'pin1',
    tool_labels: { search: 'read' },
    never_allowed: ['update_page'],
  };
  server.use(
    http.get('*/v1/runners/:install/proxy/runner/v1/projects/project/mcp-tools', () =>
      HttpResponse.json({
        items: [
          {
            id: connection.id,
            name: connection.name,
            pinned_snapshot_id: 'pin1',
            tools: [{ name: 'search', label: 'read' }],
          },
        ],
      }),
    ),
  );
  render(
    <QueryClientProvider client={new QueryClient()}>
      <AgentConfiguration
        agent={{ id: 'agent1', name: 'checker', description: '', status: 'active', group_id: 'g' }}
        admin
        canEdit
        groups={[{ id: 'g', name: 'Primary' }]}
        profiles={[{ id: 'p', name: 'Default model' }]}
        status={{ status: 'online', install_id: 'runner', groups: [], runners: [] }}
        connections={[connection]}
        projectId="project"
        credentials={[credential]}
        save={save}
        operate={async () => {}}
      />
    </QueryClientProvider>,
  );
  fireEvent.change(screen.getByLabelText('Default model profile'), { target: { value: 'p' } });
  fireEvent.click(await screen.findByRole('checkbox', { name: 'Workspace' }));
  expect(screen.queryByRole('checkbox', { name: /update_page/ })).toBeNull();
  fireEvent.click(screen.getByRole('checkbox', { name: /search/ }));
  fireEvent.click(screen.getByRole('button', { name: 'Save configuration' }));
  expect(await screen.findByRole('alert')).toHaveTextContent('Select each required credential');
  expect(save).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole('checkbox', { name: /Workspace credential/ }));
  fireEvent.click(screen.getByRole('button', { name: 'Save configuration' }));
  await waitFor(() =>
    expect(save).toHaveBeenCalledWith(
      expect.objectContaining({
        profiles: ['p'],
        mcp: [{ server_id: 'workspace', tool: 'search' }],
        credentials: [{ credential_id: 'oauth_key', delivery: 'mcp' }],
      }),
    ),
  );
});

test('proposal_shows_diff_and_approves_in_one_click', async () => {
  const decide = vi.fn(async () => {});
  const items = [
    {
      id: 'change',
      kind: 'agent.change',
      status: 'open',
      diff: { project_id: 'p_1', instructions: 'Check the logs' },
    },
  ];
  const view = render(<ProposalList items={items} canDecide={false} decide={decide} />);
  expect(screen.queryByRole('button', { name: 'Approve' })).toBeNull();
  view.rerender(<ProposalList items={items} canDecide decide={decide} />);
  expect(screen.getByText(/\+.*Check the logs/)).toBeVisible();
  fireEvent.click(screen.getByRole('button', { name: 'Approve' }));
  await waitFor(() => expect(decide).toHaveBeenCalledExactlyOnceWith('change', 'approve'));
});

test('template_creates_agent_from_short_form', async () => {
  const create = vi.fn(async () => {});
  render(
    <TemplateForm
      groups={[{ id: 'g_1', name: 'Default' }]}
      profiles={[{ id: 'profile_1', name: 'Fast' }]}
      create={create}
    />,
  );
  fireEvent.change(screen.getByLabelText('Template'), { target: { value: 'data-qa' } });
  fireEvent.change(screen.getByLabelText('Agent name'), { target: { value: 'warehouse-qa' } });
  fireEvent.click(screen.getByRole('button', { name: 'Create agent' }));
  await waitFor(() =>
    expect(create).toHaveBeenCalledWith(
      expect.objectContaining({
        name: 'warehouse-qa',
        group_id: 'g_1',
        profiles: ['profile_1'],
        instructions: expect.stringMatching(/quality/i),
      }),
    ),
  );
});

test('activity_shows_cost_success_and_idle_stops', () => {
  render(
    <Activity
      activity={{
        busy_ms: 9000,
        idle_ms: 1000,
        succeeded: 9,
        completed: 10,
        idle_stops: 2,
        p50_ms: 48000,
        success_rate: 0.9,
      }}
      cost={12.4}
    />,
  );
  expect(screen.getByText('$12.40')).toBeVisible();
  expect(screen.getByText('Success rate').parentElement).toHaveTextContent('90%');
  expect(screen.getByText('Median step duration').parentElement).toHaveTextContent('48s');
  expect(screen.getByText('Busy time (24 hours)').parentElement).toHaveTextContent('9s');
  expect(screen.getByText('Idle stops')).toBeVisible();
  expect(screen.getByText('2')).toBeVisible();
});

test('version_diff_lists_runs_per_version', () => {
  render(
    <Versions
      versions={[
        {
          id: 'v2',
          version: 2,
          config: { instructions: 'New instruction' },
          run_count: 51,
          runs: [{ execution_id: 'run_2', title: 'Verify deploy' }],
        },
        { id: 'v1', version: 1, config: { instructions: 'Old instruction' } },
      ]}
    />,
  );
  expect(screen.getByText(/\+.*New instruction/)).toBeVisible();
  expect(screen.getByText(/-.*Old instruction/)).toBeVisible();
  expect(screen.getByText('51 runs using this version')).toBeVisible();
  expect(screen.getByRole('link', { name: 'Verify deploy' })).toHaveAttribute('href', '/x/run_2');
});

test('project admins see the scoped agent audit', () => {
  const client = new QueryClient();
  const path = '/projects/p_1/agents/agent_1';
  client.setQueryData(['runner-proxy', 'ri_1', `${path}/audit?after=0`], {
    items: [
      { seq: '1', at: '2026-11-20T00:00:00Z', action: 'agent.updated', actor: { id: 'user-1' } },
    ],
  });
  const status = { status: 'offline' as const, install_id: 'ri_1', groups: [], runners: [] };
  const view = render(
    <QueryClientProvider client={client}>
      <AgentAudit status={status} path={path} projectAdmin />
    </QueryClientProvider>,
  );
  expect(screen.getByRole('table', { name: 'Agent audit' })).toHaveTextContent('agent.updated');
  view.rerender(
    <QueryClientProvider client={client}>
      <AgentAudit status={status} path={path} projectAdmin={false} />
    </QueryClientProvider>,
  );
  expect(screen.queryByText('agent.updated')).toBeNull();
});

// R1-D2: the agent page lists the agent's approval decisions and abstentions.
test('agent_page_lists_recent_decisions_and_abstentions', () => {
  const approval = (extra: Record<string, unknown>) => ({
    schema_revision: 1,
    status: 'waiting',
    prompt: 'Ship?',
    options: [
      { id: 'ship', title: 'Ship it', auto: false },
      { id: 'hold', title: 'Hold', auto: false },
    ],
    eligible_recipients: [],
    escalation_recipients: [],
    escalated: false,
    quorum: 1,
    decisions: [],
    ...extra,
  });
  const report = {
    header: { title: 'Release 2.4.0' },
    sections: [
      {
        step_run_id: 'sr_gate',
        title: 'Release gate',
        approval: approval({
          step_run_id: 'sr_gate',
          status: 'succeeded',
          decisions: [
            {
              by: 'agent',
              principal: 'runner:rin_1',
              agent_name: 'release-reviewer',
              model: 'claude-sonnet',
              session_id: 'clm_2',
              option: 'ship',
              comment: 'Canary is healthy [call 1].',
              evidence: ['[call 1]'],
            },
          ],
        }),
      },
      {
        step_run_id: 'sr_hold',
        title: 'Second gate',
        approval: approval({
          step_run_id: 'sr_hold',
          agent_notes: [
            {
              lease_id: 'clm_3',
              principal: 'runner:rin_1',
              agent_name: 'release-reviewer',
              model: 'claude-sonnet',
              reason: 'No canary data yet.',
              at_ms: 1,
            },
          ],
        }),
      },
      { step_run_id: 'sr_task', title: 'Plain task', approval: null },
    ],
  };
  const recent = [
    { step_run_id: 'sr_gate', execution_id: 'exe_1' },
    { step_run_id: 'sr_hold', execution_id: 'exe_1' },
    { step_run_id: 'sr_task', execution_id: 'exe_1' },
    { step_run_id: 'sr_gate', execution_id: 'exe_1' },
  ];
  const rows = decisionRows(
    recent,
    new Map([['exe_1', report as unknown as Schemas['ExecutionDocumentView']]]),
  );
  expect(rows.map((row) => row.state)).toEqual(['decided', 'abstained']);
  render(<Activity activity={{ busy_ms: 0, idle_ms: 0, recent }} decisions={rows} project="web" />);
  const table = screen.getByRole('table', { name: 'Recent approval decisions' });
  expect(table).toHaveTextContent('Release gate · Release 2.4.0');
  expect(table).toHaveTextContent('Decided: Ship it');
  expect(table).toHaveTextContent('claude-sonnet');
  expect(table).toHaveTextContent('Canary is healthy [call 1].');
  expect(table).toHaveTextContent('Evidence: [call 1]');
  expect(table).toHaveTextContent('Abstained');
  expect(table).toHaveTextContent('No canary data yet.');
  expect(
    screen.getAllByRole('link', { name: 'Transcript' }).map((link) => link.getAttribute('href')),
  ).toContain('/p/web/agents/transcripts/sr_gate');
});

test('agent_page_says_when_there_are_no_decisions', () => {
  render(<Activity activity={{ busy_ms: 0, idle_ms: 0 }} decisions={[]} />);
  expect(
    screen.getByText('This agent has not decided or abstained on an approval lately.'),
  ).toBeVisible();
});
