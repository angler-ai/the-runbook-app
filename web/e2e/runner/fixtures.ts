import type { components } from '../../src/api/schema.d.ts';
type Schemas = components['schemas'];
import type {
  Budget,
  Connection,
  Environment,
  Project,
  Snapshot,
} from '../../src/features/runner/resources.ts';

export const org = 'org_sample';
export const install = 'install_sample';
export const group = 'group_sample';
export const project = {
  id: 'project_sample',
  slug: 'operations',
  title: 'Operations',
} satisfies Project;
export const fixtureProject = { ...project, org_id: org } satisfies Schemas['ProjectView'];
export const fixtureUser = { id: 'user_sample', name: 'Alex Rivera', email: 'alex@example.test' };
export const fixtureMe = {
  user: fixtureUser,
  org: { id: org, role: 'owner', name: 'Northstar', slug: 'northstar' },
  credential: { actor_kind: 'human', kind: 'session', scopes: ['runner:read', 'runner:admin'] },
  teams: [],
};
export const fixtureStatus = {
  status: 'online',
  version: '0.1.1',
  install_id: install,
  last_heartbeat_at: Date.parse('2026-11-22T18:42:00Z'),
  queued_steps: 2,
  groups: [{ id: group, name: 'Primary', projects: [project.id] }],
  runners: [
    {
      id: 'runner_sample',
      group_id: group,
      name: 'US West',
      status: 'ready',
      running: 3,
      capacity: 12,
    },
  ],
};
export const environment = {
  id: 'environment_sample',
  name: 'Warehouse analytics',
  description: 'Python and reporting tools for warehouse quality checks.',
  version: 3,
  version_id: 'environment_version_sample',
  is_default: false,
  image: 'registry.example.test/warehouse:stable',
  image_digest: 'registry.example.test/warehouse@sha256:' + '5da4'.repeat(16),
  image_approved: false,
  setup: '#!/bin/sh\nset -eu\npython --version',
  setup_timeout_s: 120,
  env: { REPORT_FORMAT: 'summary' },
  credential_env: { WAREHOUSE_API_KEY: 'warehouse_key' },
  size: 'medium',
  timeout_s: 600,
  egress: [{ host: 'warehouse.example.test', label: 'read' }],
  budgets: {
    cost_per_day_usd: 20,
    cost_per_month_usd: 400,
    concurrency: 4,
    max_step_cost_usd: 2,
    max_run_cost_usd: 8,
    alert_threshold_percent: 80,
  },
  created_at: '2026-11-20T12:00:00Z',
  updated_at: '2026-11-22T12:00:00Z',
} satisfies Environment;
const history = [
  5, 7, 6, 7, 7, 5, 9, 11, 8, 10, 11, 6, 8, 12, 11, 10, 9, 11, 7, 12, 12, 8, 11, 13, 11, 13, 14, 11,
  16, 20,
].map((spend_usd, index) => ({
  date: new Date(Date.UTC(2026, 10, index - 6)).toISOString().slice(0, 10),
  spend_usd,
}));
export const budgetRows: Budget[] = [
  {
    scope: 'org',
    id: org,
    name: 'Organization',
    project_id: null,
    period: 'day',
    cap: 60,
    spend: 28.54,
    percentage: 47.6,
    daily_spend: history,
    alert_threshold_percent: 80,
    blocked: [],
  },
  {
    scope: 'environment',
    id: environment.id,
    name: environment.name,
    project_id: null,
    period: 'day',
    cap: 20,
    spend: 20,
    percentage: 100,
    daily_spend: history,
    alert_threshold_percent: 80,
    blocked_count: 26,
    blocked: Array.from({ length: 25 }, (_, i) => ({
      step_run_id: i ? `step_sample_${i}` : 'step_sample',
      execution_id: 'exe_sample',
      title: i ? `Check warehouse partition ${i}` : 'Check warehouse freshness',
      reason: 'Environment Warehouse analytics: daily budget $20.00 reached',
    })),
  },
  {
    scope: 'environment',
    id: environment.id,
    name: environment.name,
    project_id: null,
    period: 'concurrency',
    cap: 4,
    spend: 3,
    percentage: 75,
    daily_spend: [],
    alert_threshold_percent: null,
    blocked: [],
  },
  {
    scope: 'project',
    id: project.id,
    name: project.title,
    project_id: project.id,
    period: 'month',
    cap: 400,
    spend: 82.15,
    percentage: 20.5,
    daily_spend: history,
    alert_threshold_percent: 75,
    blocked: [],
  },
];
export const tools = [
  {
    name: 'search',
    description: 'Find pages in the connected workspace.',
    inputSchema: { type: 'object', properties: { query: { type: 'string' } } },
  },
  {
    name: 'update_page',
    description: 'Update a page’s title or properties.',
    inputSchema: { type: 'object', properties: { title: { type: 'string' } } },
  },
];
export const existingConnections: Connection[] = [
  {
    id: 'github',
    name: 'GitHub read-only',
    url: 'https://api.githubcopilot.com/mcp/readonly',
    transport: 'streamable-http',
    sharing: { projects: [project.id] },
    pinned_snapshot_id: 'github_tools',
    pinned_tool_count: 1,
    tool_labels: { search_repositories: 'read' },
  },
  {
    id: 'warehouse',
    name: 'Warehouse catalog',
    url: 'https://warehouse.example.test/mcp',
    transport: 'streamable-http',
    sharing: { projects: [project.id] },
    pinned_snapshot_id: 'warehouse_tools',
    pinned_tool_count: 2,
    tool_labels: { search: 'read', update_page: 'write' },
    never_allowed: ['update_page'],
    drift: true,
  },
];
export const pinnedSnapshots: Record<string, Snapshot> = {
  github: {
    snapshot_id: 'github_tools',
    hash: 'github-hash',
    tools: [{ ...tools[0]!, name: 'search_repositories' }],
  },
  warehouse: { snapshot_id: 'warehouse_tools', hash: 'warehouse-hash', tools },
};
export const warehouseDiscovery: Snapshot = {
  snapshot_id: 'warehouse_discovery',
  hash: 'warehouse-discovery-hash',
  tools: [
    { ...tools[0]!, inputSchema: { ...tools[0]!.inputSchema, required: ['query'] } },
    { ...tools[0]!, name: 'list_sources' },
  ],
};
export const warehouseChanges = {
  added: ['list_sources'],
  changed: [{ name: 'search', before: tools[0]!, schema_changed: true }],
  removed: [tools[1]!],
};
export const pickerConnections = [
  {
    id: 'github',
    name: 'GitHub read-only',
    pinned_snapshot_id: 'github_tools',
    tools: [{ name: 'search_repositories', label: 'read' }],
  },
  {
    id: 'warehouse',
    name: 'Warehouse catalog',
    pinned_snapshot_id: 'warehouse_tools',
    tools: [{ name: 'search', label: 'read' }],
  },
];
export const credentialRow = {
  id: 'warehouse_key',
  name: 'Warehouse gateway key',
  type: 'api-key',
  label: 'read',
  delivery: ['proxy'],
  sharing: { projects: [project.id] },
  status: 'active',
  groups: 'all',
  config: {
    hosts: ['warehouse.example.test'],
    header_name: 'Authorization',
    header_format: 'Bearer {}',
    secret_source: {
      type: 'gcp-secret-manager',
      resource: 'projects/northstar/secrets/warehouse',
      version: 'latest',
    },
  },
  secret_meta: {},
};
export const provider = {
  id: 'anthropic_sample',
  name: 'Anthropic',
  type: 'anthropic',
  auth: 'stored',
  endpoint: 'https://api.anthropic.com',
  models: [{ id: 'claude-sonnet-4-6', input_usd_per_million: 3, output_usd_per_million: 15 }],
  sharing: { projects: 'all' },
};
export const profile = {
  id: 'profile_sample',
  name: 'Default Claude',
  provider_id: provider.id,
  model: provider.models[0]!.id,
  options: {},
  sharing: { projects: 'all' },
  verified_at: '2026-11-22T17:12:00Z',
};
export const policy = {
  org_concurrency: 20,
  org_cost_per_day_usd: 60,
  org_cost_per_month_usd: 1200,
  alert_threshold_percent: 80,
  agent_concurrency: 5,
  agent_max_cost_usd: 1,
  run_max_cost_usd: 10,
  credential_max_age_days: 90,
  time_zone: 'America/Los_Angeles',
  project_limits: { [project.id]: { cost_per_month_usd: 400, alert_threshold_percent: 75 } },
  default_egress: [{ host: 'warehouse.example.test', label: 'read' }],
};
export const agent = {
  id: 'agent_sample',
  name: 'warehouse-checker',
  description: 'Checks daily warehouse freshness and quality.',
  group_id: group,
  status: 'active',
  config: {
    profiles: [profile.id],
    environment_id: environment.id,
    environment_version_id: environment.version_id,
    instructions: 'Check the warehouse for data freshness and missing values.',
    credentials: [{ credential_id: credentialRow.id, delivery: 'proxy' }],
    mcp: [{ server_id: 'warehouse', tool: 'search' }],
    limits: { max_cost_usd: 1, max_tokens: 16384, timeout_s: 600 },
  },
};
export const audit = {
  chain: { ok: true },
  items: [
    {
      seq: '2',
      at: '2026-11-22T12:20:00Z',
      action: 'catalog.pinned',
      resource_type: 'mcp-servers',
      actor: { kind: 'user', id: fixtureUser.name },
    },
    {
      seq: '1',
      at: '2026-11-22T12:12:00Z',
      action: 'environment.updated',
      resource_type: 'environment',
      actor: { kind: 'user', id: fixtureUser.name },
    },
  ],
};
export const proposals = [
  {
    id: 'proposal_sample',
    kind: 'catalog.change',
    status: 'open',
    reason: 'The reporting workflow needs more headroom during its weekly refresh.',
    diff: { resource: 'policy', change: { agent_concurrency: 6 } },
  },
];
