import { useQueries } from '@tanstack/react-query';
import { Bot } from 'lucide-react';
import { EmptyState } from '../../ui/layout';
import { StatusPill } from '../../ui/status/StatusPill';
import { dollars } from '../execution/RunnerStep';
import type { Agent } from '../agents/AgentConfiguration';
import {
  runnerError,
  runnerQueryOptions,
  useRunnerQuery,
  type Items,
  type RunnerStatus,
} from './api';
import { Rows, sentenceCase } from './presentation';
import type { Connection, Environment, Project } from './resources';
import { Problem, textLink } from './runnerForms';

export function OrgAgents({ status, projects }: { status: RunnerStatus; projects: Project[] }) {
  const results = useQueries({
    queries: projects.map((project) =>
      runnerQueryOptions<Items<Agent>>(status, `/projects/${project.id}/agents`),
    ),
  });
  const environments = useRunnerQuery<Items<Environment>>(status, '/environments');
  const connections = useRunnerQuery<Items<Connection>>(status, '/mcp-servers');
  const usage = useRunnerQuery<Items<{ resource_id: string; cost_usd: string }>>(
    status,
    '/usage?level=agent&period=day',
  );
  const rows = results.flatMap((result, index) =>
    (result.data?.items ?? []).map((agent) => {
      const project = projects[index]!;
      const environment = environments.data?.items.find(
        (item) => item.id === agent.config?.environment_id,
      );
      const names = [...new Set(agent.config?.mcp?.map((tool) => tool.server_id))].map(
        (id) =>
          connections.data?.items.find((item) => item.id === id)?.name ?? 'Unavailable connection',
      );
      const total = usage.data?.items
        .filter((item) => item.resource_id === agent.id)
        .reduce((sum, item) => sum + Number(item.cost_usd), 0);
      return {
        id: agent.id,
        cells: [
          <a className={textLink} href={`/p/${project.slug}/agents/${agent.id}`}>
            {agent.name}
          </a>,
          project.title,
          <StatusPill
            status={agent.status === 'active' ? 'succeeded' : 'paused'}
            label={sentenceCase(agent.status)}
          />,
          environment?.name ??
            (agent.config?.environment_id ? 'Unavailable environment' : 'Organization default'),
          names.join(', ') || 'None',
          total == null ? '—' : dollars(total),
        ],
      };
    }),
  );
  const failure = results.find((result) => result.error)?.error;
  return (
    <div className="grid gap-3">
      <Problem>{failure && runnerError(failure)}</Problem>
      {rows.length ? (
        <Rows
          label="Organization agents"
          stackOnMobile
          columns={['Agent', 'Project', 'Status', 'Environment', 'Connections', 'Today’s spend']}
          rows={rows}
        />
      ) : results.some((result) => result.isPending) ? (
        <p role="status">Loading agents…</p>
      ) : (
        !failure && (
          <EmptyState
            icon={Bot}
            title="No runner agents"
            description="Create an agent in a project that has runner access."
            action={
              <a
                className={textLink}
                href={projects[0] ? `/p/${projects[0].slug}/agents` : '/runner/overview'}
              >
                {projects[0] ? 'Open project Agents' : 'Open runner setup'}
              </a>
            }
          />
        )
      )}
    </div>
  );
}
