// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, expect, test } from 'vitest';
import { BudgetTable } from './Budgets';
import { readVariables } from './Environments';
import { createMemoryHistory } from '@tanstack/react-router';
import { http, HttpResponse } from 'msw';
import { App } from '../../App';
import { sessionStore } from '../../api/token-store';
import { createAppRouter } from '../../routes/router';
import { server } from '../../test/msw/server';
import * as fixture from '../../../e2e/runner/fixtures';

afterEach(() => {
  cleanup();
  sessionStore.clearCookieSession();
});

test('shows spend, alert threshold and the reason an environment budget blocked a step', () => {
  render(
    <BudgetTable
      timeZone="UTC"
      items={[
        {
          scope: 'environment',
          id: 'env1',
          name: 'Angler data',
          project_id: null,
          period: 'day',
          cap: 20,
          spend: 20,
          percentage: 100,
          alert_threshold_percent: 80,
          daily_spend: [{ date: '2026-11-22', spend_usd: 20 }],
          blocked_count: 26,
          blocked: [
            {
              step_run_id: 'step1',
              execution_id: 'run1',
              title: 'Fetch data',
              reason: 'Angler data daily budget reached',
            },
          ],
        },
      ]}
      projects={[]}
    />,
  );
  expect(screen.getByRole('link', { name: 'Angler data' })).toHaveAttribute(
    'href',
    '/runner/environments/env1#environment-budgets',
  );
  expect(screen.getByText('100%')).toBeVisible();
  expect(screen.getByText('80%')).toBeVisible();
  expect(screen.getByText('Angler data daily budget reached')).toBeInTheDocument();
  expect(screen.getByText('1 of 26 blocked steps')).toBeInTheDocument();
  expect(screen.getByText('Fetch data').closest('a')).toHaveAttribute(
    'href',
    '/x/run1?view=steps&step=step1',
  );
  expect(screen.getByText(/Daily spend started and ended at/)).toBeInTheDocument();
});

test('rejects reserved environment variables before saving', () => {
  const fields = new FormData();
  fields.append('env:name', 'PYTHONPATH');
  fields.append('env:value', '/work');
  expect(() => readVariables(fields, 'env')).toThrow('Variable name PYTHONPATH is reserved');
});

const proxyReads: Record<string, unknown> = {
  '/mcp-servers': { items: fixture.existingConnections },
  '/mcp-servers/warehouse': fixture.existingConnections[1],
  '/mcp-servers/warehouse/tools': { snapshot: fixture.pinnedSnapshots.warehouse },
  '/mcp-servers/warehouse/drift': {
    latest: fixture.warehouseDiscovery,
    changes: fixture.warehouseChanges,
  },
  [`/projects/${fixture.project.id}/mcp-tools`]: { items: fixture.pickerConnections },
  '/credentials': { items: [fixture.credentialRow] },
  '/providers': { items: [fixture.provider] },
  '/profiles': { items: [fixture.profile] },
  '/environments': { items: [fixture.environment] },
  [`/environments/${fixture.environment.id}`]: fixture.environment,
  '/environment-registries': { items: ['registry.example.test'] },
  '/budgets': { items: fixture.budgetRows, time_zone: fixture.policy.time_zone },
  '/policy': fixture.policy,
  '/groups': { items: [{ group_id: fixture.group, name: 'Primary' }] },
  '/audit': fixture.audit,
  '/proposals': { items: fixture.proposals },
  [`/projects/${fixture.project.id}/agents`]: { items: [fixture.agent] },
  [`/projects/${fixture.project.id}/agents/${fixture.agent.id}`]: fixture.agent,
  '/usage': { items: [{ kind: 'model', resource_id: fixture.agent.id, cost_usd: '28.54' }] },
};

function openPage(path: string, scope = ['runner:read', 'runner:admin'], reads = proxyReads) {
  sessionStore.setCookieSession();
  server.use(
    http.get('*/v1/me', () =>
      HttpResponse.json({
        ...fixture.fixtureMe,
        credential: { ...fixture.fixtureMe.credential, scopes: scope },
      }),
    ),
    http.get('*/v1/orgs', () => HttpResponse.json({ items: [fixture.fixtureMe.org] })),
    http.get('*/v1/orgs/:org/projects', () =>
      HttpResponse.json({ items: [fixture.fixtureProject] }),
    ),
    http.get('*/v1/orgs/:org/users', () => HttpResponse.json({ items: [] })),
    http.get('*/v1/orgs/:org/teams', () => HttpResponse.json({ items: [] })),
    http.get('*/v1/projects/:project/members', () => HttpResponse.json({ items: [] })),
    http.get('*/v1/projects/:project/runbooks', () => HttpResponse.json({ items: [] })),
    http.get('*/v1/projects/:project/executions', () => HttpResponse.json({ items: [] })),
    http.get('*/v1/orgs/:org/runner', () => HttpResponse.json(fixture.fixtureStatus)),
    http.get('*/v1/runners/:install/proxy/runner/v1/*', ({ request }) => {
      const path = new URL(request.url).pathname.split('/proxy/runner/v1')[1]!;
      if (reads[path] !== undefined) return HttpResponse.json(reads[path]);
      return HttpResponse.json(
        { error: { code: 'fixture.unexpected_read', message: 'Unknown test resource' } },
        { status: 404 },
      );
    }),
  );
  const router = createAppRouter(createMemoryHistory({ initialEntries: [path] }));
  render(<App router={router} />);
  return router;
}

test.each([
  ['/runner/overview', 'Setup checklist', 'heading'],
  ['/runner/connections', 'Warehouse catalog', 'heading'],
  ['/runner/connections/warehouse?tab=tools', 'Pinned tools', 'table'],
  ['/runner/models', 'Anthropic prices in USD per million tokens', 'table'],
  ['/runner/environments', 'Environments', 'table'],
  [`/runner/environments/${fixture.environment.id}`, 'Environment budgets', 'heading'],
  ['/runner/budgets', 'Runner budgets', 'table'],
  ['/runner/policy', 'Organization budgets', 'heading'],
  ['/runner/agents', 'Organization agents', 'table'],
  ['/runner/audit', 'Runner audit', 'table'],
  ['/runner/credentials', 'Credentials', 'table'],
  ['/runner/proposals', 'Change runner catalog', 'heading'],
  ['/p/operations/agents/agent_sample?tab=configuration', 'Connections and tools', 'heading'],
])('renders %s from runner resources', async (path, name, role) => {
  openPage(path);
  expect(await screen.findByRole(role, { name })).toBeVisible();
  if (path.startsWith('/p/')) {
    expect(screen.getByRole('link', { name: 'Agents' })).toHaveAttribute(
      'href',
      '/p/operations/agents',
    );
  }
});

test('the old Runner settings address opens the corresponding Runner page', async () => {
  const router = openPage('/settings/organization/runner?tab=providers');
  expect(await screen.findByRole('heading', { name: 'Models' })).toBeVisible();
  expect(router.state.location.pathname).toBe('/runner/models');
});

test('a runner reader can view connections without the connect action', async () => {
  openPage('/runner/connections', ['runner:read']);
  expect(await screen.findByRole('heading', { name: 'Warehouse catalog' })).toBeVisible();
  expect(screen.queryByRole('link', { name: 'Connect a system' })).toBeNull();
});

test('shows the installation queue and drops the duplicate health table', async () => {
  openPage('/runner/overview');
  expect(await screen.findByText('2 queued')).toBeVisible();
  expect(screen.queryByRole('table', { name: 'Runner capacity' })).toBeNull();
});

test('uses a neutral sign-in pill for public connections', async () => {
  openPage('/runner/connections');
  const card = await screen.findByRole('link', { name: /Warehouse catalog/ });
  expect(
    within(card).getByText('No sign-in needed').closest('[data-slot="status-pill"]'),
  ).toHaveAttribute('data-status', 'skipped');
  expect(within(card).getByText('Per tool')).toBeVisible();
});

test('shows pinned schemas and changes from the latest discovery after a direct visit', async () => {
  openPage('/runner/connections/warehouse?tab=tools');
  const table = await screen.findByRole('table', { name: 'Pinned tools' });
  expect(within(table).getByText('update_page')).toBeVisible();
  expect(within(table).getAllByText('Input schema')).toHaveLength(2);
  expect(await screen.findByRole('heading', { name: 'Changes since pin' })).toBeVisible();
  expect(screen.getByText('Added')).toBeVisible();
  expect(screen.getByText('Removed')).toBeVisible();
  fireEvent.click(screen.getByText('Changed'));
  expect(screen.getByText('Input schema changed')).toBeVisible();
  expect(screen.getByText(/"required":/)).toBeVisible();
  fireEvent.click(screen.getByRole('button', { name: 'Review discovered tools' }));
  expect(await screen.findByRole('table', { name: 'Discovered tools' })).toBeVisible();
  expect(screen.getByRole('checkbox', { name: 'Allow list_sources' })).not.toBeChecked();
});

test('loads grouped project tool choices without visiting a connection first', async () => {
  openPage('/p/operations/agents/agent_sample?tab=configuration');
  const group = await screen.findByRole('checkbox', { name: 'GitHub read-only' });
  fireEvent.click(group);
  const tool = screen.getByRole('checkbox', { name: /search_repositories/ });
  expect(tool).toBeVisible();
  expect(tool.closest('label')).toHaveTextContent('Read');
  expect(screen.queryByRole('checkbox', { name: /update_page/ })).toBeNull();
});

test('a saved tool removed from the pin can still be removed from the agent', async () => {
  const agent = {
    ...fixture.agent,
    config: {
      ...fixture.agent.config,
      mcp: [{ server_id: 'warehouse', tool: 'retired_search' }],
    },
  };
  openPage('/p/operations/agents/agent_sample?tab=configuration', undefined, {
    ...proxyReads,
    [`/projects/${fixture.project.id}/agents/${agent.id}`]: agent,
  });
  fireEvent.click(await screen.findByRole('button', { name: 'Remove retired_search' }));
  expect(screen.queryByText('retired_search')).toBeNull();
});

test('audit search matches the displayed resource name', async () => {
  openPage('/runner/audit');
  await screen.findByText('MCP server');
  fireEvent.change(screen.getByRole('searchbox', { name: 'Filter audit events' }), {
    target: { value: 'MCP server' },
  });
  expect(screen.getByText('Pinned connection tools')).toBeVisible();
  expect(screen.queryByText('Environment updated')).toBeNull();
});

test('the budgets page follows the cursor to show more budgets', async () => {
  const [first, ...rest] = fixture.budgetRows;
  openPage('/runner/budgets');
  server.use(
    http.get('*/v1/runners/:install/proxy/runner/v1/budgets', ({ request }) => {
      const cursor = new URL(request.url).searchParams.get('cursor');
      if (cursor === 'page-2')
        return HttpResponse.json({
          items: rest,
          time_zone: fixture.policy.time_zone,
          next_cursor: null,
        });
      return HttpResponse.json({
        items: [first],
        time_zone: fixture.policy.time_zone,
        next_cursor: 'page-2',
      });
    }),
  );
  const more = await screen.findByRole('button', { name: 'Show more budgets' });
  const rows = () =>
    within(screen.getByRole('table', { name: 'Runner budgets' })).getAllByRole('row').length;
  const before = rows();
  fireEvent.click(more);
  await waitFor(() => expect(rows()).toBe(before + rest.length));
  expect(screen.queryByRole('button', { name: 'Show more budgets' })).toBeNull();
});

test('R01 bulk read labeling cannot prevent blocking a write tool', async () => {
  openPage('/runner/connections/warehouse?tab=tools', undefined, {
    ...proxyReads,
    '/mcp-servers/warehouse': { ...fixture.existingConnections[1], never_allowed: [] },
  });
  const tool = await screen.findByLabelText('Classification for update_page');
  fireEvent.click(screen.getByRole('button', { name: 'Label all read' }));
  expect(tool).toHaveValue('write');
  fireEvent.change(tool, { target: { value: 'read' } });
  fireEvent.click(screen.getByRole('button', { name: 'Block all writes' }));
  expect(screen.getByRole('checkbox', { name: 'Allow update_page' })).not.toBeChecked();
  fireEvent.click(screen.getByRole('button', { name: 'Save tool access' }));
  expect(screen.getByRole('dialog', { name: 'Review tool access' })).toHaveTextContent(
    'update_page · Allowed → Blocked',
  );
});

test('R03 and R15 keep the preset group mounted and protocol controls in Advanced', async () => {
  openPage('/runner/connections/new');
  const notion = await screen.findByRole('radio', { name: /Notion/ });
  const meta = screen.getByRole('radio', { name: /Meta Ads/ });
  fireEvent.click(meta);
  expect(notion).toBeInTheDocument();
  expect(meta).toBeChecked();
  expect(screen.getByText('Advanced')).toBeVisible();
  expect(screen.getByLabelText('Transport').closest('details')).not.toHaveAttribute('open');
});

test('R05 previews a project cap removal before saving', async () => {
  openPage('/runner/policy');
  const cap = await screen.findByRole('textbox', { name: 'Operations monthly cap' });
  fireEvent.change(cap, { target: { value: '' } });
  fireEvent.click(screen.getAllByRole('button', { name: 'Save limits' })[0]!);
  expect(await screen.findByRole('dialog', { name: 'Review budget increases' })).toHaveTextContent(
    'Operations',
  );
});

test('R06 previews registry additions and removals', async () => {
  openPage('/runner/environments');
  fireEvent.change(await screen.findByLabelText('Registry hostnames'), {
    target: { value: 'next.example.test' },
  });
  fireEvent.click(screen.getByRole('button', { name: 'Save registries' }));
  const dialog = await screen.findByRole('dialog', { name: 'Review registry access' });
  expect(dialog).toHaveTextContent('Add next.example.test');
  expect(dialog).toHaveTextContent('Remove registry.example.test');
});

test('R08 associates and focuses the invalid scoped budget field', async () => {
  openPage('/runner/policy');
  const cap = await screen.findByRole('textbox', { name: 'Operations daily cap' });
  fireEvent.change(cap, { target: { value: '-1' } });
  fireEvent.click(screen.getAllByRole('button', { name: 'Save limits' })[0]!);
  await waitFor(() => expect(cap).toHaveFocus());
  expect(cap).toHaveAttribute('aria-invalid', 'true');
  expect(cap).toHaveAccessibleDescription(/Operations.*Daily cap.*between 0/i);
});

test('R09 groups Runner destinations', async () => {
  openPage('/runner/overview');
  const nav = await screen.findByRole('navigation', { name: 'Runner pages' });
  expect(within(nav).getByRole('group', { name: 'Monitoring' })).toBeInTheDocument();
  expect(screen.getByRole('link', { name: 'All agents' })).toHaveAttribute(
    'href',
    '/runner/agents',
  );
});

test('R10 filters pinned tools by name without changing tool access', async () => {
  openPage('/runner/connections/warehouse?tab=tools');
  const search = await screen.findByRole('searchbox', { name: 'Search tools' });
  fireEvent.change(search, { target: { value: 'update_page' } });
  expect(screen.getByRole('checkbox', { name: 'Allow update_page' })).toBeInTheDocument();
  expect(screen.queryByRole('checkbox', { name: 'Allow search' })).toBeNull();
});

test('R11 shows a retry when the connection credential inventory fails', async () => {
  openPage('/runner/connections', undefined, { ...proxyReads, '/credentials': undefined });
  expect(await screen.findByText('Host connections could not be loaded.')).toBeVisible();
  expect(screen.getByRole('button', { name: 'Retry credentials' })).toBeVisible();
  expect(screen.getByRole('heading', { name: 'Warehouse catalog' })).toBeVisible();
});

test('R13 offers the pending digest for the cluster operator', async () => {
  openPage(`/runner/environments/${fixture.environment.id}`);
  expect(
    await screen.findByRole('button', { name: 'Copy digest for cluster operator' }),
  ).toBeVisible();
  expect(screen.getByText(/before running/)).toBeVisible();
});

test('R16 gives environment budgets a local link and a persistent save action', async () => {
  openPage(`/runner/environments/${fixture.environment.id}`);
  const sections = await screen.findByRole('navigation', { name: 'Environment sections' });
  expect(within(sections).getByRole('link', { name: 'Budgets' })).toHaveAttribute(
    'href',
    '#environment-budgets',
  );
  expect(screen.getByRole('button', { name: 'Save environment' }).parentElement).toHaveClass(
    'sticky',
  );
});

test('R04 carries the provider destination into the inline key form and preserves its draft', async () => {
  openPage('/runner/models');
  fireEvent.click(await screen.findByRole('button', { name: 'Add provider' }));
  fireEvent.change(screen.getByLabelText('Provider name'), {
    target: { value: 'Operations models' },
  });
  fireEvent.click(screen.getByRole('button', { name: 'Add provider key' }));
  const dialog = await screen.findByRole('dialog', { name: 'Add provider key' });
  expect(dialog).toHaveTextContent('https://api.anthropic.com');
  expect(within(dialog).getByLabelText('API key')).toBeVisible();
  fireEvent.click(within(dialog).getByRole('button', { name: 'Close' }));
  expect(screen.getByLabelText('Provider name')).toHaveValue('Operations models');
});

test('R12 shows which workspace and project an OAuth disconnect affects', async () => {
  const credential = {
    ...fixture.credentialRow,
    id: 'oauth-warehouse',
    name: 'Warehouse OAuth',
    type: 'mcp-oauth',
    status: 'active',
    sharing: { projects: [fixture.project.id] },
    config: { url: 'https://warehouse.example.test/mcp', issuer: 'https://warehouse.example.test' },
    oauth_status: { connected_account: 'Northstar workspace' },
  };
  openPage('/runner/connections/warehouse', undefined, {
    ...proxyReads,
    '/mcp-servers/warehouse': { ...fixture.existingConnections[1], credential_id: credential.id },
    [`/credentials/${credential.id}`]: credential,
  });
  fireEvent.click(await screen.findByRole('button', { name: 'Disconnect' }));
  const impact = screen.getByRole('group', { name: 'Disconnect impact' });
  expect(impact).toHaveTextContent('Northstar workspace');
  expect(impact).toHaveTextContent('Operations');
  expect(impact).toHaveTextContent('Future calls that need this sign-in will fail');
});

test('R02 requires and saves the guided tool and credential, then links to its checklist', async () => {
  const connection = { ...fixture.existingConnections[1], credential_id: fixture.credentialRow.id };
  const credential = {
    ...fixture.credentialRow,
    name: 'Warehouse sign-in',
    delivery: ['mcp', 'proxy'],
  };
  const agent = { ...fixture.agent, config: { ...fixture.agent.config, mcp: [] } };
  const detail = `/projects/${fixture.project.id}/agents/${agent.id}`;
  openPage('/p/operations/agents/agent_sample?tab=configuration&connection=warehouse', undefined, {
    ...proxyReads,
    [detail]: agent,
    '/mcp-servers': { items: [connection] },
    '/credentials': { items: [credential] },
    [detail + '/tool-calls']: { items: [] },
  });
  let saved: unknown;
  server.use(
    http.patch('*/v1/runners/:install/proxy/runner/v1' + detail, async ({ request }) => {
      saved = await request.json();
      return HttpResponse.json(agent);
    }),
  );
  await screen.findByRole('heading', { name: 'Set up Warehouse catalog for warehouse-checker' });
  fireEvent.click(await screen.findByRole('checkbox', { name: /search/ }));
  const grant = await screen.findByRole('checkbox', { name: /Warehouse sign-in · MCP/ });
  fireEvent.click(screen.getByRole('button', { name: 'Save configuration' }));
  await waitFor(() => expect(grant).toHaveFocus());
  expect(saved).toBeUndefined();
  fireEvent.click(grant);
  fireEvent.click(screen.getByRole('button', { name: 'Save configuration' }));
  expect(
    await screen.findByText('Agent warehouse-checker can now use Warehouse catalog search.'),
  ).toBeVisible();
  expect(saved).toMatchObject({
    mcp: [{ server_id: 'warehouse', tool: 'search' }],
    credentials: expect.arrayContaining([{ credential_id: credential.id, delivery: 'mcp' }]),
  });
});

test('shows archived transcript entries with the read-only date', async () => {
  openPage('/p/operations/agents/transcripts/step_archive', undefined, {
    ...proxyReads,
    '/transcripts/step_archive': {
      items: [{ seq: 1, claim_id: 'claim', entry: 'Archived output' }],
      archived: true,
      archived_at: '2026-11-23',
    },
  });
  expect(await screen.findByText('Archived output')).toBeVisible();
  expect(screen.getByText('Archived on 2026-11-23; read-only.')).toBeVisible();
});

test('shows archived tool calls on the agent Tools tab with their step links', async () => {
  const path = `/projects/${fixture.project.id}/agents/${fixture.agent.id}/tool-calls`;
  openPage('/p/operations/agents/agent_sample?tab=tools', undefined, {
    ...proxyReads,
    [path]: {
      archived: true,
      archived_at: '2026-11-23',
      items: [{
        id: '42',
        server_id: 'warehouse',
        tool: 'search',
        step_run_id: 'step_archive',
        created_at: '2026-10-21T00:00:00Z',
        archived: true,
        archived_at: '2026-11-23',
      }],
    },
  });
  const history = await screen.findByRole('region', { name: 'Archived tool calls' });
  expect(within(history).getByText('Archived on 2026-11-23; read-only.')).toBeVisible();
  expect(within(history).getByRole('link', { name: 'step_archive' })).toHaveAttribute(
    'href',
    '/p/operations/agents/transcripts/step_archive',
  );
});
