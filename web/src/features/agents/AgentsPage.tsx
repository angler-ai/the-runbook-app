import { useQueries } from '@tanstack/react-query';
import { useParams, useNavigate, useSearch, Link } from '@tanstack/react-router';
import { useState } from 'react';
import { useMe } from '../../api/hooks';
import { PageHeader } from '../../ui/layout';
import { Button } from '../../ui/primitives/button';
import { Tabs, TabsList, TabsTrigger, TabsContent } from '../../ui/primitives/tabs';
import { useOrgContext } from '../context/OrgContext';
import { useMyProjectRole } from '../execution/hooks';
import { dollars } from '../execution/RunnerStep';
import {
  runnerError,
  runnerQueryOptions,
  useRunnerMutation,
  useRunnerQuery,
  useRunnerStatus,
  type Items,
  type Row,
  type RunnerStatus,
} from '../runner/api';
import { destination, type Credential } from '../runner/Credentials';
import {
  sharedWith,
  type AgentToolCall,
  type Connection,
  type Environment,
} from '../runner/resources';
import { OfflineBanner, projectAllowed, Rows } from '../runner/presentation';
import {
  Activity,
  decisionRows,
  rate,
  useDecisionReports,
  type ActivityData,
} from './AgentActivity';
import { Bindings } from './AgentBindings';
import { AgentConfiguration, type Agent } from './AgentConfiguration';
import { Versions, type AgentVersion } from './AgentVersions';
import { Proposals } from './Proposals';
import { TemplateForm } from './Templates';

type Profile = {
  id: string;
  name: string;
  model: string;
  sharing?: { projects: string | string[] };
};
type Usage = { resource_id: string; cost_usd: string | number };

function weeklyCosts(results: (Items<Usage> | undefined)[]) {
  const totals = new Map<string, number>();
  if (results.some((result) => result === undefined)) return undefined;
  for (const result of results) {
    for (const item of result?.items ?? []) {
      totals.set(item.resource_id, (totals.get(item.resource_id) ?? 0) + Number(item.cost_usd));
    }
  }
  return totals;
}

export function AgentAudit({
  status,
  path,
  projectAdmin,
}: {
  status: RunnerStatus;
  path: string;
  projectAdmin: boolean;
}) {
  const [after, setAfter] = useState('0');
  const audit = useRunnerQuery<
    Items<{ seq: string; at: string; action: string; actor: { id?: string } }>
  >(status, `${path}/audit?after=${after}`, projectAdmin);
  if (!projectAdmin)
    return <p className="text-fg-muted">Project admins can read the agent audit log.</p>;
  if (audit.error)
    return (
      <p role="alert" className="text-danger">
        {runnerError(audit.error)}
      </p>
    );
  const entries = audit.data?.items ?? [];
  return (
    <div className="grid gap-3">
      <Rows
        label="Agent audit"
        columns={['Time', 'Action', 'Actor']}
        rows={entries.map((item) => ({
          id: item.seq,
          cells: [new Date(item.at).toLocaleString(), item.action, item.actor?.id ?? '—'],
        }))}
      />
      <div className="flex gap-2">
        {after !== '0' && (
          <Button variant="secondary" onClick={() => setAfter('0')}>
            First page
          </Button>
        )}
        {entries.length === 1000 && (
          <Button variant="secondary" onClick={() => setAfter(entries.at(-1)!.seq)}>
            Next page
          </Button>
        )}
      </div>
    </div>
  );
}

const detailTabs = [
  ['activity', 'Activity'],
  ['configuration', 'Configuration'],
  ['tools', 'Tools'],
  ['credentials', 'Credentials'],
  ['versions', 'Versions'],
  ['audit', 'Audit'],
] as const;

export function AgentsPage() {
  const params = useParams({ strict: false }) as { project?: string; agent?: string };
  const { tab, connection } = useSearch({ strict: false }) as { tab?: string; connection?: string };
  const { resolveProject, isLoading } = useOrgContext();
  const project = resolveProject(params.project);
  const me = useMe();
  const statusQuery = useRunnerStatus(me.data?.org.id);
  const role = useMyProjectRole(project?.id, me.data?.user?.id);
  if (isLoading || statusQuery.isLoading) return <p role="status">Loading agents…</p>;
  if (statusQuery.error)
    return (
      <p role="alert" className="text-danger">
        {runnerError(statusQuery.error)}
      </p>
    );
  const status = statusQuery.data;
  if (!project || !status || !projectAllowed(status, project.id))
    return <p className="text-fg-muted">This project is not allowed on the runner.</p>;
  const canWrite =
    !project.archived &&
    me.data?.credential.scopes?.includes('runner:admin') &&
    me.data?.credential.actor_kind === 'human';
  return (
    <ProjectAgents
      key={project.id}
      project={project}
      status={status}
      asOf={statusQuery.dataUpdatedAt}
      selectedId={params.agent}
      tab={tab}
      guidedConnection={connection}
      admin={Boolean(canWrite && role === 'admin')}
      edit={Boolean(canWrite && (role === 'admin' || role === 'editor'))}
      projectAdmin={role === 'admin'}
    />
  );
}

function ProjectAgents({
  project,
  status,
  asOf,
  selectedId,
  tab,
  guidedConnection,
  admin,
  edit,
  projectAdmin,
}: {
  project: { id: string; slug: string; title: string };
  status: RunnerStatus;
  asOf: number;
  selectedId?: string;
  tab?: string;
  guidedConnection?: string;
  admin: boolean;
  edit: boolean;
  projectAdmin: boolean;
}) {
  const navigate = useNavigate();
  const path = `/projects/${project.id}/agents`;
  const listQuery = useRunnerQuery<Items<Agent>>(status, path);
  const [newAgent, setNewAgent] = useState(false);
  const templateCredentials = useRunnerQuery<Items<Credential>>(
    status,
    '/credentials',
    newAgent && admin,
  );

  const [page, setPage] = useState(0);
  const profiles = useRunnerQuery<Items<Profile>>(status, '/profiles');
  const shared = (profiles.data?.items ?? []).filter(
    ({ sharing }) =>
      sharing?.projects === 'all' ||
      (Array.isArray(sharing?.projects) && sharing.projects.includes(project.id)),
  );
  const environments = useRunnerQuery<Items<Environment>>(
    status,
    '/environments',
    admin || Boolean(selectedId),
  );
  const connections = useRunnerQuery<Items<Connection>>(
    status,
    '/mcp-servers',
    Boolean(selectedId || guidedConnection),
  );
  const sharedConnections =
    connections.data?.items.filter((item) => sharedWith(item.sharing, project.id)) ?? [];
  const guided = sharedConnections.find(({ id }) => id === guidedConnection);
  const agents = listQuery.data?.items ?? [];
  const visible = selectedId ? [] : agents.slice(page * 20, page * 20 + 20);
  const detail = useRunnerQuery<Agent>(status, `${path}/${selectedId ?? '-'}`, Boolean(selectedId));
  const selected = detail.data ?? agents.find((agent) => agent.id === selectedId);
  const days = Array.from({ length: 7 }, (_, offset) =>
    new Date(asOf - offset * 86_400_000).toISOString().slice(0, 10),
  );
  const usage = useQueries({
    queries: days.map((date) =>
      runnerQueryOptions<Items<Usage>>(status, `/usage?level=agent&period=day&date=${date}`),
    ),
  });
  const cost = weeklyCosts(usage.map((day) => day.data));
  const summaries = useQueries({
    queries: visible.map((agent) =>
      runnerQueryOptions<ActivityData>(status, `${path}/${agent.id}/activity`),
    ),
  });
  const groups = status.groups.filter((group) => group.projects.includes(project.id));
  const mutation = useRunnerMutation(status);
  const online = status.status === 'online';
  return (
    <div className="w-full">
      <PageHeader
        title={selected?.name ?? 'Agents'}
        description={selected?.description || undefined}
      />
      {!online && <OfflineBanner offline>{null}</OfflineBanner>}
      {selectedId ? (
        <div className="grid gap-3">
          <Link
            to="/p/$project/agents"
            params={{ project: project.slug }}
            search={guidedConnection ? { connection: guidedConnection } : {}}
            className="w-fit text-sm text-accent underline"
          >
            All agents
          </Link>
          {selected ? (
            <AgentDetail
              key={selected.id}
              agent={selected}
              status={status}
              path={`${path}/${selected.id}`}
              slug={project.slug}
              tab={tab}
              cost={cost?.get(selected.id) ?? (cost ? 0 : undefined)}
              groups={groups}
              profiles={shared}
              environments={environments.data?.items ?? []}
              connections={sharedConnections}
              guided={guided}
              guidedConnection={guidedConnection}
              projectId={project.id}
              admin={admin}
              edit={edit && online}
              projectAdmin={projectAdmin}
            />
          ) : (
            <p role={detail.error ? 'alert' : 'status'} className="text-fg-muted">
              {detail.error
                ? runnerError(detail.error)
                : online
                  ? 'Loading agent…'
                  : 'No cached details for this agent.'}
            </p>
          )}
        </div>
      ) : (
        <div className="grid gap-4">
          {guided && (
            <section className="rounded-lg border border-border bg-surface p-4">
              <h2 className="font-semibold">
                Grant {guided.name} tools in {project.title}
              </h2>
              <p className="text-sm text-fg-muted">
                {admin
                  ? 'Choose an agent below or create a new one.'
                  : 'A project administrator can grant tools and credentials.'}
              </p>
            </section>
          )}
          {listQuery.error && (
            <p role="alert" className="text-danger">
              {runnerError(listQuery.error)}
            </p>
          )}
          {listQuery.isLoading && <p role="status">Loading agents…</p>}
          {admin && (
            <Button className="w-fit" disabled={!online} onClick={() => setNewAgent(!newAgent)}>
              {newAgent ? 'Cancel new agent' : 'New agent'}
            </Button>
          )}
          {newAgent && admin && (
            <TemplateForm
              groups={groups}
              profiles={shared}
              environments={environments.data?.items ?? []}
              credentials={(templateCredentials.data?.items ?? []).filter(
                (item) => item.status !== 'revoked' && sharedWith(item.sharing, project.id),
              )}
              disabled={!online}
              create={async (input) => {
                const result = (await mutation.mutateAsync({
                  path,
                  method: 'POST',
                  body: input,
                })) as { id: string };
                await navigate({
                  to: '/p/$project/agents/$agent',
                  params: { project: project.slug, agent: result.id },
                  search: guidedConnection
                    ? { tab: 'configuration', connection: guidedConnection }
                    : {},
                });
                setNewAgent(false);
              }}
            />
          )}
          <Rows
            label="Project agents"
            columns={['Name', 'Status', 'Model', 'Running', 'Success rate', '7-day cost']}
            rows={visible.map((agent, index) => {
              const activity = summaries[index]?.data;
              const profile = profiles.data?.items.find(
                ({ id }) => id === agent.config?.profiles?.[0],
              );
              return {
                id: agent.id,
                cells: [
                  <Link
                    to="/p/$project/agents/$agent"
                    params={{ project: project.slug, agent: agent.id }}
                    search={
                      guidedConnection ? { tab: 'configuration', connection: guidedConnection } : {}
                    }
                    className="font-medium text-accent underline"
                  >
                    {agent.name}
                  </Link>,
                  agent.status,
                  profile
                    ? `${profile.name} (${profile.model})`
                    : agent.config?.profiles?.length
                      ? 'Unavailable profile'
                      : '—',
                  activity?.running ?? '—',
                  rate(activity?.success_rate),
                  cost ? dollars(cost.get(agent.id) ?? 0) : '—',
                ],
              };
            })}
          />
          {(page > 0 || (page + 1) * 20 < agents.length) && (
            <div className="flex gap-2">
              <Button variant="secondary" disabled={page === 0} onClick={() => setPage(page - 1)}>
                Previous page
              </Button>
              <Button
                variant="secondary"
                disabled={(page + 1) * 20 >= agents.length}
                onClick={() => setPage(page + 1)}
              >
                Next page
              </Button>
            </div>
          )}
          <section className="grid gap-3">
            <h2 className="font-semibold">Proposals</h2>
            <Proposals
              status={status}
              canDecide={admin}
              project={project.id}
              before={(item) => {
                const current = agents.find((agent) => agent.id === item.target_id);
                if (!current) return undefined;
                const previous: Row = {
                  ...current.config,
                  name: current.name,
                  status: current.status,
                  group_id: current.group_id,
                  description: current.description,
                };
                return Object.fromEntries(
                  Object.keys(item.diff)
                    .filter((key) => key !== 'project_id')
                    .map((key) => [key, previous[key]]),
                );
              }}
            />
          </section>
        </div>
      )}
    </div>
  );
}

function AgentDetail({
  agent,
  status,
  path,
  slug,
  tab,
  cost,
  groups,
  profiles,
  environments,
  connections,
  guided,
  guidedConnection,
  projectId,
  admin,
  edit,
  projectAdmin,
}: {
  agent: Agent;
  status: RunnerStatus;
  path: string;
  slug: string;
  tab?: string;
  cost?: number;
  groups: { id: string; name: string }[];
  profiles: Profile[];
  environments: Environment[];
  connections: Connection[];
  guided?: Connection;
  guidedConnection?: string;
  projectId: string;
  admin: boolean;
  edit: boolean;
  projectAdmin: boolean;
}) {
  const navigate = useNavigate({ from: '/p/$project/agents/$agent' });
  const selectedTab = detailTabs.some(([id]) => id === tab) ? tab : 'activity';
  const activity = useRunnerQuery<ActivityData>(
    status,
    `${path}/activity`,
    selectedTab === 'activity',
  );
  // R1-D2: decision runs claim the approval's step run; their outcome is
  // on the run's report.
  const decisionReports = useDecisionReports(activity.data?.recent, selectedTab === 'activity');
  const calls = useRunnerQuery<Items<AgentToolCall>>(
    status,
    `${path}/tool-calls`,
    selectedTab === 'tools',
  );
  const versions = useRunnerQuery<Items<AgentVersion>>(
    status,
    `${path}/versions`,
    selectedTab === 'versions',
  );
  const credentials = useRunnerQuery<Items<Credential>>(
    status,
    '/credentials',
    selectedTab === 'credentials' ||
      (selectedTab === 'configuration' && admin && status.status === 'online'),
  );
  const mutation = useRunnerMutation(status);
  const save = async (body: Row) => {
    await mutation.mutateAsync({ path, method: 'PATCH', body });
  };
  const manage = admin && status.status === 'online';
  const tools = agent.ceiling?.tools ?? [];
  const credentialBindings = agent.config?.credentials ?? [];
  const readError = (error: unknown) =>
    error ? (
      <p role="alert" className="text-danger">
        {runnerError(error)}
      </p>
    ) : null;
  return (
    <Tabs
      value={selectedTab}
      onValueChange={(value) =>
        void navigate({ search: { tab: value, connection: guidedConnection }, replace: true })
      }
    >
      <TabsList aria-label="Agent details">
        {detailTabs.map(([id, label]) => (
          <TabsTrigger key={id} value={id}>
            {label}
          </TabsTrigger>
        ))}
      </TabsList>
      <TabsContent value="activity">
        {readError(activity.error)}
        <Activity
          activity={activity.data}
          cost={cost}
          project={slug}
          decisions={decisionRows(activity.data?.recent ?? [], decisionReports)}
        />
      </TabsContent>
      <TabsContent value="configuration">
        {guidedConnection && !guided && (
          <p role="status" className="mb-2 text-sm text-fg-muted">
            The requested connection must be shared with this project before you can grant its
            tools.
          </p>
        )}
        <AgentConfiguration
          agent={agent}
          guided={guided}
          projectSlug={slug}
          canEdit={edit}
          admin={admin}
          groups={groups}
          profiles={profiles}
          environments={environments}
          connections={connections}
          projectId={projectId}
          status={status}
          credentials={(credentials.data?.items ?? []).filter(
            (item) => item.status !== 'revoked' && sharedWith(item.sharing, projectId),
          )}
          save={save}
          operate={async (action) => {
            await mutation.mutateAsync({
              path: action === 'pause' ? `${path}/pause` : path,
              method: action === 'pause' ? 'POST' : 'DELETE',
            });
          }}
        />
      </TabsContent>
      <TabsContent value="tools">
        {readError(calls.error)}
        <Bindings
          kind="tools"
          editable={false}
          initial={agent.config?.mcp ?? []}
          rows={tools.map((tool) => {
            const matching = calls.data?.items.filter(
              (call) => call.tool === tool.name && call.server_id === tool.server_id,
            );
            return {
              id: `${tool.server_id}/${tool.name}`,
              cells: [
                <>
                  <span className="font-medium">{tool.name}</span>
                  <p className="text-xs text-fg-muted">
                    {connections.find((item) => item.id === tool.server_id)?.name ??
                      'Unavailable connection'}
                  </p>
                </>,
                tool.label,
                matching?.length ?? '—',
                matching?.filter((call) => call.error).length ?? '—',
              ],
            };
          })}
        />
        {calls.data?.archived && (
          <section className="mt-4 grid gap-2" aria-label="Archived tool calls">
            <h3 className="text-sm font-medium">Archived tool calls</h3>
            <ul className="grid gap-2">
              {calls.data.items.filter((call) => call.archived).map((call) => (
                <li key={call.id} className="rounded-md border border-border p-3 text-sm">
                  <p>{call.tool} · {call.error ? 'Failed' : 'Succeeded'}</p>
                  <Link
                    to="/p/$project/agents/transcripts/$step"
                    params={{ project: slug, step: call.step_run_id }}
                    search={{ claim_id: undefined }}
                    className="text-accent underline"
                  >
                    {call.step_run_id}
                  </Link>
                  <p className="text-xs text-fg-muted">Archived on {call.archived_at}; read-only.</p>
                </li>
              ))}
            </ul>
          </section>
        )}
      </TabsContent>
      <TabsContent value="credentials">
        {readError(credentials.error)}
        <Bindings
          kind="credentials"
          editable={manage}
          initial={credentialBindings}
          choices={(credentials.data?.items ?? []).filter(
            (item) => item.status !== 'revoked' && sharedWith(item.sharing, projectId),
          )}
          save={async (bindings) => save({ credentials: bindings })}
          rows={credentialBindings.map((binding) => {
            const record = credentials.data?.items.find(({ id }) => id === binding.credential_id);
            const name =
              record?.name ??
              agent.ceiling?.credentials?.find(({ id }) => id === binding.credential_id)?.name;
            return {
              id: `${binding.credential_id}/${binding.delivery}`,
              cells: [
                name ?? 'Unavailable credential',
                binding.delivery,
                record ? destination(record) : '—',
              ],
            };
          })}
        />
      </TabsContent>
      <TabsContent value="versions">
        {readError(versions.error)}
        <Versions versions={versions.data?.items ?? []} />
      </TabsContent>
      <TabsContent value="audit">
        <AgentAudit status={status} path={path} projectAdmin={projectAdmin} />
      </TabsContent>
    </Tabs>
  );
}
