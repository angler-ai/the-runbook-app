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
import { OfflineBanner, SetupCard } from './presentation';
import { Problem, selectClasses, textLink } from './runnerForms';

const pages = [
  ['overview', 'Overview', 'Runner health, workload and setup.'],
  ['connections', 'Connections', 'Manage external systems and the tools agents can use.'],
  ['models', 'Models', 'Manage providers, model prices and profiles.'],
  ['environments', 'Environments', 'Configure the sandboxes in which your agents work.'],
  ['budgets', 'Budgets', 'Track spend, concurrency and blocked work across every budget.'],
  ['policy', 'Policy', 'Set organization limits and agent defaults.'],
  ['agents', 'Agents', 'See runner agents across your projects.'],
  ['credentials', 'Credentials', 'Manage the credentials your runner uses to sign in.'],
  ['audit', 'Audit', 'Review changes to runner configuration and access.'],
  ['proposals', 'Proposals', 'Review changes proposed by agents.'],
] as const;

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
  const { tab } = useSearch({ strict: false }) as { tab?: string };
  const [page, title, description] =
    pages.find(([id]) => id === pathname.split('/')[2]) ?? pages[0];
  const admin = me.data?.org.role === 'admin' || me.data?.org.role === 'owner';
  const readable = me.data?.credential.scopes?.includes('runner:read');
  const editable = Boolean(
    admin && me.data?.credential.scopes?.includes('runner:admin') && status?.status === 'online',
  );
  if (me.isPending || (readable && query.isPending)) return <p role="status">Loading runner…</p>;
  if (!readable || !org) return <Problem>You do not have access to the runner.</Problem>;
  if (query.error) return <Problem>{runnerError(query.error)}</Problem>;
  if (!status?.install_id || status.status === 'none') return <SetupCard admin={admin} />;
  const content = () => {
    if (status.status !== 'online' && page !== 'overview') return null;
    switch (page) {
      case 'overview':
        return <Overview status={status} org={org} projects={projects} editable={editable} />;
      case 'models':
        return <Models status={status} editable={editable} projects={projects} />;
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
          {pages.map(([id, label]) => (
            <option key={id} value={id}>
              {label}
            </option>
          ))}
        </select>
      </label>
      <nav
        aria-label="Runner pages"
        className="mb-5 hidden flex-wrap gap-x-1 border-b border-border sm:flex"
      >
        {pages.map(([id, label]) => (
          <Link
            key={id}
            to={`/runner/${id}`}
            aria-current={id === page ? 'page' : undefined}
            className={`shrink-0 border-b-2 px-3 py-2.5 text-sm font-medium ${id === page ? 'border-accent text-accent' : 'border-transparent text-fg-muted hover:text-fg'}`}
          >
            {label}
          </Link>
        ))}
      </nav>
      {environment && (
        <Link to="/runner/environments" className={textLink + ' mb-4 inline-block text-sm'}>
          All environments
        </Link>
      )}
      {status.status === 'offline' && <OfflineBanner offline>{null}</OfflineBanner>}
      {content()}
    </div>
  );
}
