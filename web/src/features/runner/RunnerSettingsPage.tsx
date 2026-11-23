import { Link, useLocation, useNavigate, useParams, useSearch } from '@tanstack/react-router';
import { useMe } from '../../api/hooks';
import { PageHeader } from '../../ui/layout';
import { Proposals } from '../agents/Proposals';
import { useOrgContext } from '../context/OrgContext';
import { runnerError, useRunnerStatus } from './api';
import { Audit } from './Audit';
import { Models } from './Models';
import { PolicyPage } from './Policy';
import { ConnectionPage, Connections } from './Connections';
import { Credentials } from './Credentials';
import { Overview } from './Overview';
import { OrgAgents } from './OrgAgents';
import { Budgets } from './Budgets';
import { EnvironmentPage, Environments } from './Environments';
import { SetupCard } from './presentation';
import { Problem, selectClasses, textLink } from './runnerForms';

const pages = [
  ['overview', 'Overview', 'Runner health, workload and setup.'],
  ['connections', 'Connections', 'Manage external systems and the tools agents can use.'],
  ['models', 'Models', 'Manage providers, model prices and profiles.'],
  ['environments', 'Environments', 'Configure the sandboxes in which your agents work.'],
  ['budgets', 'Budgets', 'Track spend, concurrency and blocked work across every budget.'],
  ['policy', 'Policy', 'Set organization limits and agent defaults.'],
  ['agents', 'All agents', 'See runner agents across your projects.'],
  ['credentials', 'Credentials', 'Manage the credentials your runner uses to sign in.'],
  ['audit', 'Audit', 'Review changes to runner configuration and access.'],
  ['proposals', 'Proposals', 'Review changes proposed by agents.'],
] as const;

const sections = [
  ['Monitoring', ['overview', 'budgets', 'audit', 'agents']],
  ['Integrations', ['connections', 'credentials']],
  ['Configuration', ['models', 'environments', 'policy']],
  ['Review', ['proposals']],
] as const;

function labelFor(id: string) {
  return pages.find(([page]) => page === id)?.[1];
}

export function RunnerSettingsPage() {
  const navigate = useNavigate();
  const me = useMe();
  const { projects } = useOrgContext();
  const org = me.data?.org.id;
  const query = useRunnerStatus(org);
  const status = query.data;
  const pathname = useLocation({ select: (location) => location.pathname });
  const { environment, connection } = useParams({ strict: false }) as {
    environment?: string;
    connection?: string;
  };
  const {
    tab,
    project,
    agent,
    connection: setupConnection,
  } = useSearch({ strict: false }) as {
    tab?: string;
    project?: string;
    agent?: string;
    connection?: string;
  };
  const [page, title, description] =
    pages.find(([id]) => id === pathname.split('/')[2]) ?? pages[0];
  const admin = me.data?.org.role === 'admin' || me.data?.org.role === 'owner';
  const readable = me.data?.credential.scopes?.includes('runner:read');
  const editable = Boolean(
    admin && me.data?.credential.scopes?.includes('runner:admin') && status?.status === 'online',
  );
  if (me.isPending || (readable && query.isPending)) return <p role="status">Loading runner…</p>;
  if (!readable || !org) return <Problem>You do not have access to the runner.</Problem>;
  if (query.error)
    return (
      <Problem>
        {runnerError(query.error)}{' '}
        <button type="button" className={textLink} onClick={() => void query.refetch()}>
          Retry runner status
        </button>
      </Problem>
    );
  if (!status?.install_id || status.status === 'none') return <SetupCard admin={admin} />;
  const content = () => {
    if (status.status !== 'online' && page !== 'overview') return null;
    switch (page) {
      case 'overview':
        return (
          <Overview
            status={status}
            org={org}
            projects={projects}
            editable={editable}
            setupProject={project}
            setupAgent={agent}
            setupConnection={setupConnection}
          />
        );
      case 'models':
        return <Models status={status} org={org} editable={editable} projects={projects} />;
      case 'connections':
        return connection ? (
          <ConnectionPage
            id={connection}
            tab={tab}
            status={status}
            org={org}
            editable={editable}
            projects={projects}
          />
        ) : (
          <Connections status={status} org={org} editable={editable} projects={projects} />
        );
      case 'credentials':
        return <Credentials status={status} org={org} editable={editable} projects={projects} />;
      case 'environments':
        return environment ? (
          <EnvironmentPage id={environment} status={status} editable={editable} />
        ) : (
          <Environments status={status} editable={editable} owner={me.data?.org.role === 'owner'} />
        );
      case 'budgets':
        return <Budgets status={status} projects={projects} />;
      case 'policy':
        return <PolicyPage status={status} editable={editable} projects={projects} />;
      case 'agents':
        return <OrgAgents status={status} projects={projects} />;
      case 'audit':
        return <Audit status={status} />;
      case 'proposals':
        return (
          <Proposals
            status={status}
            canDecide={editable && me.data?.credential.actor_kind === 'human'}
          />
        );
      default:
        return null;
    }
  };
  return (
    <div className="min-w-0">
      <PageHeader
        eyebrow="Runner"
        title={
          environment === 'new'
            ? 'New environment'
            : environment
              ? 'Environment settings'
              : connection === 'new'
                ? 'Connect a system'
                : connection
                  ? 'Connection details'
                  : title
        }
        description={description}
      />
      <label className="mb-4 grid gap-1 text-xs font-medium text-fg-muted sm:hidden">
        Runner page
        <select
          className={selectClasses}
          value={page}
          onChange={(event) => {
            const choice = pages.find((item) => item[0] === event.target.value);
            if (choice) void navigate({ to: `/runner/${choice[0]}` });
          }}
        >
          {sections.map(([section, ids]) => (
            <optgroup label={section} key={section}>
              {ids.map((id) => (
                <option key={id} value={id}>
                  {labelFor(id)}
                </option>
              ))}
            </optgroup>
          ))}
        </select>
      </label>
      <nav
        aria-label="Runner pages"
        className="mb-5 hidden flex-wrap gap-x-1 border-b border-border sm:flex"
      >
        {sections.map(([section, ids]) => (
          <div
            role="group"
            aria-label={section}
            key={section}
            className="min-w-0 border-r border-border pr-2 last:border-r-0"
          >
            <p aria-hidden="true" className="px-2 pt-1 text-xs text-fg-muted">
              {section}
            </p>
            <div className="flex flex-wrap">
              {ids.map((id) => (
                <Link
                  key={id}
                  to={`/runner/${id}`}
                  aria-current={id === page ? 'page' : undefined}
                  className={`border-b-2 px-2 py-2 text-sm font-medium ${id === page ? 'border-accent text-accent' : 'border-transparent text-fg-muted hover:text-fg'}`}
                >
                  {labelFor(id)}
                </Link>
              ))}
            </div>
          </div>
        ))}
      </nav>
      {environment && (
        <Link to="/runner/environments" className={textLink + ' mb-4 inline-block text-sm'}>
          All environments
        </Link>
      )}
      {status.status === 'offline' && page !== 'overview' && (
        <p role="status" className="rounded-md border border-border bg-surface p-3 text-warn">
          Runner offline. {title} details are unavailable until it reconnects.{' '}
          <Link className={textLink} to="/runner/overview" hash="runner-heartbeat">
            View last heartbeat
          </Link>
        </p>
      )}
      {status.status === 'online' && !editable && (
        <p
          className="mb-4 rounded-md border border-border bg-surface p-3 text-sm text-fg-muted"
          role="status"
        >
          View only. A runner administrator can change access.
        </p>
      )}
      {content()}
    </div>
  );
}
