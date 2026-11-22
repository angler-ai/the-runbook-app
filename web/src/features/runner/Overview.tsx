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
import { docs, panel, Rows, VaultIdentityFingerprint } from './presentation';

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
      href: '?tab=providers',
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
      <h2 className="mb-2 font-semibold">Setup checklist</h2>
      <ul className="grid gap-2 text-sm">
        {items.map(({ done, label, href }) => (
          <li key={label} className="flex items-center gap-3">
            <span className={done ? 'text-success' : 'text-fg-muted'}>
              {done ? 'Done' : 'To do'}
            </span>
            <a className="text-accent underline" href={href}>
              {label}
            </a>
          </li>
        ))}
      </ul>
    </section>
  );
}

export function Capacity({ groups, runners }: Pick<RunnerStatus, 'groups' | 'runners'>) {
  const showGroup = groups.length > 1;
  const showRunner = runners.length > 1;
  return (
    <Rows
      label="Runner capacity"
      columns={[
        ...(showGroup ? ['Group'] : []),
        ...(showRunner ? ['Runner'] : []),
        'Health',
        'Running / capacity',
        'Projects',
      ]}
      rows={groups.flatMap((group) => {
        const workers = runners.filter((runner) => runner.group_id === group.id);
        return (workers.length ? workers : [undefined]).map((runner) => ({
          id: runner?.id ?? group.id,
          cells: [
            ...(showGroup ? [group.name] : []),
            ...(showRunner ? [runner?.name ?? '—'] : []),
            runner?.status ?? 'No runners',
            runner ? `${runner.running} / ${runner.capacity}` : '—',
            group.projects.join(', ') || 'None',
          ],
        }));
      })}
    />
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
      <section className={panel}>
        <h2 className="font-semibold">Control plane</h2>
        <p className="mt-1 text-sm capitalize">
          {status.status}
          {status.version ? ` · v${status.version}` : ''}
        </p>
        <VaultIdentityFingerprint publicKey={status.vault_identity_key} />
        {status.last_heartbeat_at && (
          <p className="text-sm text-fg-muted">
            Last heartbeat {new Date(status.last_heartbeat_at).toLocaleString()}
          </p>
        )}
        <p className="mt-2 text-sm">
          Today {usage.data ? dollars(model + compute) : '—'}
          {policy.data?.org_cost_per_day_usd != null
            ? ` of ${dollars(Number(policy.data.org_cost_per_day_usd))}`
            : ''}
        </p>
        {usage.data && (
          <p className="text-sm text-fg-muted">
            Model {dollars(model)} · Compute {dollars(compute)}
          </p>
        )}
        {installType && <p className="mt-2 text-sm">Install type {installType}</p>}
        <p className="mt-2 text-sm text-fg-muted">
          Docker is a single trust domain. Anyone with access to the Docker host can reach
          everything on it.
        </p>
      </section>
      <Capacity groups={status.groups} runners={status.runners} />
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
      <Checklist
        online={status.status === 'online'}
        profiles={profiles.data?.items}
        hasProject={Boolean(firstProject)}
        project={firstProject?.slug}
        hasAgent={Boolean(agents.data?.items.length)}
        hasStep={Boolean(usage.data?.items.length)}
      />
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
