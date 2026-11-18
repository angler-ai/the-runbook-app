// UI-1: the runbooks list and its empty state's agent panel.
import { defineGallery } from '../../gallery/registry';
import type { RunbookRow } from './catalog';
import { RunbookList, type RunbookLinks } from './RunbookList';
import { AgentCreatePanel } from './RunbooksEmpty';

const NOW = Date.parse('2026-11-13T17:00:00Z');
const MIN = 60_000;

const rows: RunbookRow[] = [
  {
    runbook_id: 'rb_01JPOSTDEPLOY',
    slug: 'post-deploy-checks',
    origin: 'ui',
    archived: false,
    title: 'Post-deploy checks',
    description:
      'Agents check health, error rates and p95 latency after every deploy, then summarise.',
    tags: ['deploy', 'agentic', 'production'],
    latest_version: 3,
    draft_ahead: false,
    last_run: { id: 'exe_01JRUNNING', status: 'running', created_at_ms: NOW - 4 * MIN },
    active_runs: 1,
  },
  {
    runbook_id: 'rb_01JONBOARD',
    slug: 'customer-onboarding',
    origin: 'ui',
    archived: false,
    title: 'Customer onboarding',
    description: 'Provision, verify the customer’s first sync, and hand over to success.',
    tags: ['customers'],
    latest_version: 2,
    draft_ahead: true,
    last_run: { id: 'exe_01JWAITING', status: 'waiting', created_at_ms: NOW - 95 * MIN },
    active_runs: 1,
  },
  {
    runbook_id: 'rb_01JDBFAILOVER',
    slug: 'db-failover-drill',
    origin: 'git',
    archived: false,
    title: 'Database failover drill',
    description: 'Fail the primary over to a replica and confirm writes resume.',
    tags: ['database', 'drill'],
    latest_version: 5,
    draft_ahead: false,
    last_run: {
      id: 'exe_01JFAILED',
      status: 'failed',
      created_at_ms: NOW - 26 * 60 * MIN,
      completed_at_ms: NOW - 25 * 60 * MIN,
    },
    active_runs: 0,
  },
  {
    runbook_id: 'rb_01JCERTS',
    slug: 'cert-rotation',
    origin: 'ui',
    archived: false,
    title: 'TLS certificate rotation',
    tags: [],
    latest_version: null,
    draft_ahead: false,
    active_runs: 0,
  },
];

const links: RunbookLinks = {
  open: (row) => `#${row.slug}/runs`,
  edit: (row) => `#${row.slug}/editor`,
  versions: (row) => `#${row.slug}/versions`,
  run: (id) => `#${id}`,
};

export default [
  defineGallery({
    id: 'runbooks-list',
    group: 'Runbooks',
    title: 'RunbookList',
    examples: [
      {
        name: 'Agentic ops checks with last runs',
        render: () => (
          <RunbookList
            rows={rows}
            links={links}
            canWrite
            now={NOW}
            onStartRun={() => {}}
            onNavigate={() => {}}
            renderLink={({ href, children, ...rest }) => (
              <a href={href} {...rest}>
                {children}
              </a>
            )}
          />
        ),
      },
    ],
  }),
  defineGallery({
    id: 'runbooks-agent-create',
    group: 'Runbooks',
    title: 'Create with your AI agent',
    examples: [
      {
        name: 'CLI and MCP',
        render: () => <AgentCreatePanel origin="https://runbook.example.com" project="ops" />,
      },
    ],
  }),
];
