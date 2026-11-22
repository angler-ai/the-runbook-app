import { useNavigate, useSearch } from '@tanstack/react-router';
import { Proposals } from '../agents/Proposals';
import { useMe } from '../../api/hooks';
import { PageHeader } from '../../ui/layout';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '../../ui/primitives/tabs';
import { useOrgContext } from '../context/OrgContext';
import { runnerError, useRunnerStatus } from './api';
import { Audit } from './Audit';
import { Catalog, PolicyBudgets } from './Catalog';
import { Credentials } from './Credentials';
import { Overview } from './Overview';
import { OfflineBanner, SetupCard } from './presentation';

export function RunnerSettingsPage() {
  const me = useMe();
  const { projects } = useOrgContext();
  const org = me.data?.org.id;
  const query = useRunnerStatus(org);
  const status = query.data;
  const navigate = useNavigate({ from: '/settings/organization/runner' });
  const { tab } = useSearch({ from: '/_authenticated/settings/organization/runner' });
  const admin = me.data?.org.role === 'admin' || me.data?.org.role === 'owner';
  const editable = Boolean(admin && me.data?.credential.scopes?.includes('runner:admin'));
  if (!admin || !org) return null;
  if (!status)
    return query.error ? (
      <p role="alert">{runnerError(query.error)}</p>
    ) : (
      <p role="status">Loading runner…</p>
    );
  if (status.status === 'none' || !status.install_id) return <SetupCard admin={admin} />;
  const tabs = [
    ['overview', 'Overview'],
    ['providers', 'Model providers'],
    ['mcp', 'MCP servers'],
    ['credentials', 'Credentials'],
    ['policy', 'Policy & budgets'],
    ['audit', 'Audit'],
    ['proposals', 'Proposals'],
  ];
  return (
    <div className="w-full">
      <PageHeader title="Runner" />
      {status.status === 'offline' && <OfflineBanner offline>{null}</OfflineBanner>}
      <Tabs
        value={tabs.some(([id]) => id === tab) ? tab : 'overview'}
        onValueChange={(value) => void navigate({ search: { tab: value }, replace: true })}
      >
        <TabsList aria-label="Runner settings">
          {tabs.map(([id, label]) => (
            <TabsTrigger key={id} value={id!}>
              {label}
            </TabsTrigger>
          ))}
        </TabsList>
        <TabsContent value="overview">
          <Overview
            status={status}
            org={org}
            projects={projects}
            editable={editable && status.status === 'online'}
          />
        </TabsContent>
        <TabsContent value="providers" className="grid gap-6">
          <Catalog kind="providers" status={status} editable={editable} />
          <Catalog kind="profiles" status={status} editable={editable} />
        </TabsContent>
        <TabsContent value="mcp">
          <Catalog kind="mcp-servers" status={status} editable={editable} />
        </TabsContent>
        <TabsContent value="credentials">
          <Credentials status={status} org={org} editable={editable} />
        </TabsContent>
        <TabsContent value="policy">
          <PolicyBudgets status={status} editable={editable} />
        </TabsContent>
        <TabsContent value="audit">
          <Audit status={status} />
        </TabsContent>
        <TabsContent value="proposals">
          <Proposals
            status={status}
            canDecide={editable && me.data?.credential.actor_kind === 'human'}
          />
        </TabsContent>
      </Tabs>
    </div>
  );
}
