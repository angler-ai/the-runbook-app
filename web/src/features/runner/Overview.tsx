import {
  Activity,
  CheckCircle2,
  Circle,
  Coins,
  Layers3,
  Server,
  TriangleAlert,
} from 'lucide-react';
import { StatusPill } from '../../ui/status/StatusPill';
import type { BudgetOverview, Connection } from './resources';
import type { Credential } from './Credentials';
import { textLink } from './runnerForms';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { api, type Schemas } from '../../api/client';
import { Button } from '../../ui/primitives/button';
import { dollars } from '../execution/RunnerStep';
import {
  runnerError,
  runnerRequest,
  runnerStatusKey,
  useRunnerQuery,
  type Items,
  type Row,
  type RunnerStatus,
} from './api';
import { docs, panel, sentenceCase, VaultIdentityFingerprint } from './presentation';

export function Checklist({
  online,
  profiles,
  hasProject,
  project,
  hasAgent,
  hasStep,
}: {
  online: boolean;
  profiles?: readonly { verified_at?: string | null }[];
  hasProject: boolean;
  project?: string;
  hasAgent?: boolean;
  hasStep?: boolean;
}) {
  const projectUrl = project ? `/p/${project}` : undefined;
  const items = [
    { done: online, label: 'Runner is online', href: `${docs}/setup/` },
    {
      done: profiles?.some(({ verified_at }) => Boolean(verified_at)),
      label: 'A model profile works',
      href: '/runner/models',
    },
    { done: hasProject, label: 'A project is allowed', href: '#runner-projects' },
    {
      done: hasAgent,
      label: 'A first agent exists',
      href: projectUrl ? `${projectUrl}/agents` : `${docs}/agents/`,
    },
    {
      done: hasStep,
      label: 'A first step ran',
      href: projectUrl ? `${projectUrl}/runbooks` : `${docs}/agents/#target-an-agent`,
    },
  ];
  return (
    <section className={panel}>
      <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <h2 className="font-semibold">Setup checklist</h2>
        <span className="font-mono text-xs text-fg-muted">
          {items.filter((item) => item.done).length} of {items.length} complete
        </span>
      </div>
      <ul className="grid divide-y divide-border text-sm">
        {items.map(({ done, label, href }) => (
          <li key={label} className="flex items-center gap-3 py-2">
            {done ? (
              <CheckCircle2 className="size-4 text-success" aria-label="Done" />
            ) : (
              <Circle className="size-4 text-fg-muted" aria-label="To do" />
            )}
            <span className="sr-only">{done ? 'Done' : 'To do'}</span>
            <a className="text-accent underline" href={href}>
              {label}
            </a>
          </li>
        ))}
      </ul>
    </section>
  );
}

export function Overview({
  status,
  org,
  projects,
  editable,
}: {
  status: RunnerStatus;
  org: string;
  projects: Schemas['ProjectView'][];
  editable: boolean;
}) {
  const client = useQueryClient();
  const policy = useRunnerQuery<Row>(status, '/policy');
  const budgets = useRunnerQuery<BudgetOverview>(status, '/budgets');
  const credentials = useRunnerQuery<Items<Credential>>(status, '/credentials');
  const connections = useRunnerQuery<Items<Connection>>(status, '/mcp-servers');
  const daily = budgets.data?.items.find((item) => item.scope === 'org' && item.period === 'day');
  const running = status.runners.reduce((total, runner) => total + runner.running, 0);
  const queued = status.queued_steps;
  const warnings = [
    ...(credentials.data?.items.flatMap((item) =>
      item.status === 'reconnect_required' || item.oauth_status?.expiry_warning
        ? [
            {
              name: item.name,
              href: '/runner/credentials',
              reason: item.status === 'reconnect_required' ? 'Reconnect required' : 'Expires soon',
            },
          ]
        : [],
    ) ?? []),
    ...(connections.data?.items
      .filter((item) => item.drift)
      .map((item) => ({
        name: item.name,
        href: `/runner/connections/${item.id}?tab=tools`,
        reason: 'Tools changed since pin',
      })) ?? []),
  ];
  const groups = useRunnerQuery<
    Items<{
      group_id: string;
      sandbox?: { backend?: string; type?: string };
      idle_stops?: number;
      busy_ms?: number;
      idle_ms?: number;
    }>
  >(status, '/groups');
  const usage = useRunnerQuery<Items<{ kind: string; cost_usd: string }>>(
    status,
    '/usage?level=org&period=day',
  );
  const profiles = useRunnerQuery<Items<{ verified_at?: string | null }>>(status, '/profiles');
  const firstProject = projects.find(({ id }) =>
    status.groups.some((group) => group.projects.includes(id)),
  );
  const agents = useRunnerQuery<Items<{ id: string }>>(
    status,
    `/projects/${firstProject?.id ?? '-'}/agents`,
    Boolean(firstProject),
  );
  const assignment = useMutation({
    mutationFn: async ({ group, projectIds }: { group: string; projectIds: string[] }) => {
      const install = status.install_id;
      if (!editable || !install || status.status !== 'online') throw new Error('Runner is offline');
      const previous = status.groups.find(({ id }) => id === group)?.projects ?? [];
      const retained = previous.filter((id) => projectIds.includes(id));
      const saveCore = (ids: string[]) =>
        api.PUT('/v1/orgs/{org}/runner/groups/{g}/projects', {
          params: { path: { org, g: group } },
          body: { project_ids: ids },
        });
      // Remove access at the core before changing the runner. Add access at
      // the core only after the runner accepts the new allow-list.
      if (retained.length !== previous.length) await saveCore(retained);
      await runnerRequest(install, `/groups/${group}`, 'PATCH', { projects: projectIds });
      if (retained.length !== projectIds.length) await saveCore(projectIds);
    },
    onSettled: async () => {
      await Promise.all([
        client.invalidateQueries({ queryKey: runnerStatusKey(org) }),
        client.invalidateQueries({ queryKey: ['runner-proxy', status.install_id, '/groups'] }),
      ]);
    },
  });
  const compute = Number(usage.data?.items.find(({ kind }) => kind === 'compute')?.cost_usd ?? 0);
  const model = Number(usage.data?.items.find(({ kind }) => kind === 'model')?.cost_usd ?? 0);
  const backend = groups.data?.items.find(
    ({ sandbox }) => sandbox?.backend || sandbox?.type,
  )?.sandbox;
  const installType = backend?.backend ?? backend?.type;
  const totals = groups.data?.items;
  const sum = (key: 'busy_ms' | 'idle_ms' | 'idle_stops') =>
    totals?.some((group) => group[key] != null)
      ? totals.reduce((total, group) => total + (group[key] ?? 0), 0)
      : undefined;
  const busy = sum('busy_ms');
  const idle = sum('idle_ms');
  return (
    <div className="grid gap-4">
      <div className="grid gap-3 sm:grid-cols-3">
        <section className={panel}>
          <h2 className="flex items-center gap-2 text-sm text-fg-muted">
            <Server className="size-4" aria-hidden /> Runner health
          </h2>
          <div className="mt-3">
            <StatusPill
              status={status.status === 'online' ? 'succeeded' : 'waiting'}
              label={status.status === 'online' ? 'Online' : 'Offline'}
            />
          </div>
          <p className="mt-2 text-xs text-fg-muted">
            {[status.version && `v${status.version}`, installType && sentenceCase(installType)]
              .filter(Boolean)
              .join(' · ') || 'Version unavailable'}
          </p>
        </section>
        <section className={panel}>
          <h2 className="flex items-center gap-2 text-sm text-fg-muted">
            <Coins className="size-4" aria-hidden /> Today’s spend
          </h2>
          <p className="mt-2 font-mono text-xl tabular-nums">
            {daily ? dollars(daily.spend) : usage.data ? dollars(model + compute) : '—'}
          </p>
          <a href="/runner/budgets" className={textLink + ' text-xs'}>
            {daily
              ? `of ${dollars(daily.cap)} daily cap`
              : policy.data?.org_cost_per_day_usd != null
                ? `of ${dollars(Number(policy.data.org_cost_per_day_usd))} daily cap`
                : 'View budgets'}
          </a>
        </section>
        <section className={panel}>
          <h2 className="flex items-center gap-2 text-sm text-fg-muted">
            <Layers3 className="size-4" aria-hidden /> Running steps
          </h2>
          <p className="mt-2 font-mono text-xl tabular-nums">{running}</p>
          <p className="text-xs text-fg-muted">
            {queued == null ? 'Queue count unavailable' : `${queued} queued`}
          </p>
        </section>
      </div>
      {warnings.length > 0 && (
        <section className={panel}>
          <h2 className="flex items-center gap-2 font-semibold">
            <TriangleAlert className="size-4 text-warn" aria-hidden /> Needs attention
          </h2>
          <ul className="mt-3 grid gap-2 text-sm">
            {warnings.map((item) => (
              <li key={item.href + item.name} className="flex flex-wrap gap-x-3">
                <a className={textLink} href={item.href}>
                  {item.name}
                </a>
                <span className="text-fg-muted">{item.reason}</span>
              </li>
            ))}
          </ul>
        </section>
      )}
      <div className="grid gap-4 lg:grid-cols-[minmax(0,1.65fr)_minmax(0,1fr)]">
        <div className="grid min-w-0 gap-4">
          <section className={panel}>
            <h2 className="flex items-center gap-2 font-semibold">
              <Activity className="size-4 text-fg-muted" aria-hidden /> Control plane
            </h2>
            {status.last_heartbeat_at && (
              <p className="mt-2 text-sm text-fg-muted">
                Last heartbeat {new Date(status.last_heartbeat_at).toLocaleString()}
              </p>
            )}
            <VaultIdentityFingerprint publicKey={status.vault_identity_key} />
          </section>
          <section className={panel}>
            <h2 className="font-semibold">Efficiency</h2>
            <p className="mt-1 text-sm text-fg-muted">
              Sandboxes busy{' '}
              {busy != null && idle != null && busy + idle > 0
                ? `${Math.round((100 * busy) / (busy + idle))}%`
                : '—'}{' '}
              · Idle stops {sum('idle_stops') ?? '—'}
            </p>
          </section>
        </div>
        <Checklist
          online={status.status === 'online'}
          profiles={profiles.data?.items}
          hasProject={Boolean(firstProject)}
          project={firstProject?.slug}
          hasAgent={Boolean(agents.data?.items.length)}
          hasStep={Boolean(usage.data?.items.length)}
        />
      </div>
      <section id="runner-projects" className={panel}>
        <h2 className="font-semibold">Allowed projects</h2>
        {status.groups.map((group) => (
          <form
            key={`${group.id}:${group.projects.join()}`}
            className="mt-3 grid gap-2"
            onSubmit={(event) => {
              event.preventDefault();
              assignment.mutate({
                group: group.id,
                projectIds: new FormData(event.currentTarget).getAll('projects').map(String),
              });
            }}
          >
            <fieldset className="grid gap-2" disabled={!editable || assignment.isPending}>
              <legend className="text-sm font-medium">{group.name}</legend>
              {projects.map((project) => (
                <label className="flex items-center gap-2 text-sm" key={project.id}>
                  <input
                    className="accent-accent"
                    type="checkbox"
                    name="projects"
                    value={project.id}
                    defaultChecked={group.projects.includes(project.id)}
                  />
                  {project.title}
                </label>
              ))}
              {editable && (
                <Button className="w-fit" type="submit" variant="secondary">
                  Save allowed projects
                </Button>
              )}
            </fieldset>
          </form>
        ))}
        {assignment.error && (
          <p role="alert" className="text-danger">
            {runnerError(assignment.error)}
          </p>
        )}
      </section>
    </div>
  );
}
