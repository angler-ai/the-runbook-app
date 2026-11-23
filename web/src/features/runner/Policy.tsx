import { useEffect, useState, type FormEvent, type ReactNode } from 'react';
import { useLocation } from '@tanstack/react-router';
import { Button } from '../../ui/primitives/button';
import { Input } from '../../ui/primitives/input';
import {
  runnerError,
  useRunnerMutation,
  useRunnerQuery,
  type Items,
  type RunnerStatus,
} from './api';
import { Field, Rows } from './presentation';
import {
  BudgetFields,
  EgressFields,
  FormSection,
  Problem,
  numberField,
  readBudgets,
  readEgress,
  requiredText,
} from './runnerForms';
import type { EgressRule, Limits, Project } from './resources';

type Policy = {
  org_concurrency: number | null;
  org_cost_per_day_usd: number | null;
  org_cost_per_month_usd: number | null;
  alert_threshold_percent: number | null;
  agent_concurrency: number;
  agent_max_cost_usd: number;
  run_max_cost_usd: number | null;
  credential_max_age_days: number | null;
  time_zone: string;
  project_limits: Record<string, Limits>;
  default_egress: EgressRule[];
};
type Group = Limits & { group_id: string; name: string };
const caps: (keyof Limits)[] = [
  'cost_per_day_usd',
  'cost_per_month_usd',
  'concurrency',
  'alert_threshold_percent',
];

function PolicyForm({
  status,
  editable,
  path,
  children,
  body,
  method = 'PUT',
}: {
  status: RunnerStatus;
  editable: boolean;
  path: string;
  children: ReactNode;
  body(data: FormData): unknown;
  method?: 'PUT' | 'PATCH';
}) {
  const mutation = useRunnerMutation(status);
  const [message, setMessage] = useState('');
  const [error, setError] = useState('');
  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setError('');
    setMessage('');
    try {
      await mutation.mutateAsync({ path, method, body: body(new FormData(event.currentTarget)) });
      setMessage('Limits saved.');
    } catch (problem) {
      setError(runnerError(problem));
    }
  }
  return (
    <form className="grid gap-3" onSubmit={(event) => void save(event)}>
      <fieldset className="grid min-w-0 gap-4" disabled={!editable || mutation.isPending}>
        {children}
      </fieldset>
      {editable && (
        <Button type="submit" className="w-fit" disabled={mutation.isPending}>
          {mutation.isPending ? 'Saving…' : 'Save limits'}
        </Button>
      )}
      <Problem>{error}</Problem>
      {message && (
        <p role="status" className="text-sm text-success">
          {message}
        </p>
      )}
    </form>
  );
}

export function PolicyPage({
  status,
  editable,
  projects,
}: {
  status: RunnerStatus;
  editable: boolean;
  projects: Project[];
}) {
  const policy = useRunnerQuery<Policy>(status, '/policy');
  const groups = useRunnerQuery<Items<Group>>(status, '/groups');
  const hash = useLocation({ select: (location) => location.hash });
  useEffect(() => {
    if (policy.data && hash) document.getElementById(hash)?.scrollIntoView?.();
  }, [hash, policy.data, groups.data]);
  if (policy.error) return <Problem>{runnerError(policy.error)}</Problem>;
  if (!policy.data) return <p role="status">Loading policy…</p>;
  const data = policy.data;
  return (
    <div className="grid gap-4">
      <PolicyForm
        status={status}
        editable={editable}
        path="/policy"
        body={(form) => {
          const limits = readBudgets(form, 'org:', caps);
          const timeZone = requiredText(form, 'time_zone', 'Budget time zone');
          try {
            new Intl.DateTimeFormat('en', { timeZone });
          } catch {
            throw new Error(
              'Budget time zone must be an IANA time zone, such as America/Los_Angeles.',
            );
          }
          return {
            org_cost_per_day_usd: limits.cost_per_day_usd,
            org_cost_per_month_usd: limits.cost_per_month_usd,
            org_concurrency: limits.concurrency,
            alert_threshold_percent: limits.alert_threshold_percent,
            agent_concurrency: numberField(form, 'agent_concurrency', 'Default agent concurrency', {
              integer: true,
              max: 2_147_483_647,
              required: true,
            }),
            agent_max_cost_usd: numberField(form, 'agent_max_cost_usd', 'Default step cap (USD)', {
              required: true,
            }),
            run_max_cost_usd: numberField(form, 'run_max_cost_usd', 'Default run cap (USD)'),
            credential_max_age_days: numberField(
              form,
              'credential_max_age_days',
              'Maximum credential age',
              { integer: true, max: 2_147_483_647 },
            ),
            time_zone: timeZone,
            default_egress: readEgress(form),
            project_limits: {
              ...data.project_limits,
              ...Object.fromEntries(
                projects.map((project) => {
                  const set = readBudgets(form, project.id + ':', caps);
                  return [
                    project.id,
                    Object.fromEntries(Object.entries(set).filter(([, value]) => value != null)),
                  ];
                }),
              ),
            },
          };
        }}
      >
        <FormSection
          id="policy-budgets"
          title="Organization budgets"
          description="These limits apply across the organization."
        >
          <BudgetFields
            prefix="org:"
            keys={caps}
            initial={{
              cost_per_day_usd: data.org_cost_per_day_usd,
              cost_per_month_usd: data.org_cost_per_month_usd,
              concurrency: data.org_concurrency,
              alert_threshold_percent: data.alert_threshold_percent,
            }}
          />
          <Field label="Budget time zone">
            <Input
              name="time_zone"
              defaultValue={data.time_zone}
              placeholder="America/Los_Angeles"
            />
          </Field>
        </FormSection>
        <FormSection id="policy-defaults" title="Agent and step defaults">
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label="Default agent concurrency">
              <Input
                inputMode="numeric"
                name="agent_concurrency"
                defaultValue={data.agent_concurrency}
              />
            </Field>
            <Field label="Default step cap (USD)">
              <Input
                inputMode="decimal"
                name="agent_max_cost_usd"
                defaultValue={data.agent_max_cost_usd}
              />
            </Field>
            <Field label="Default run cap (USD)">
              <Input
                inputMode="decimal"
                name="run_max_cost_usd"
                defaultValue={data.run_max_cost_usd ?? ''}
              />
            </Field>
            <Field label="Maximum credential age (days)">
              <Input
                inputMode="numeric"
                name="credential_max_age_days"
                defaultValue={data.credential_max_age_days ?? ''}
              />
            </Field>
          </div>
        </FormSection>
        <FormSection
          title="Project limits"
          description="Project caps apply alongside organization and runner group budgets."
        >
          <Rows
            label="Project limits"
            stackOnMobile
            columns={[
              'Project',
              'Daily cap (USD)',
              'Monthly cap (USD)',
              'Concurrent steps',
              'Alert threshold (%)',
            ]}
            rows={projects.map((project) => ({
              id: project.id,
              cells: [
                <span id={`project-${project.id}`} className="font-medium">
                  {project.title}
                </span>,
                ...caps.map((key, index) => (
                  <Input
                    key={key}
                    aria-label={`${project.title} ${['daily cap', 'monthly cap', 'concurrency', 'alert threshold'][index]}`}
                    name={project.id + ':' + key}
                    inputMode="decimal"
                    defaultValue={data.project_limits?.[project.id]?.[key] ?? ''}
                    className="min-w-20"
                  />
                )),
              ],
            }))}
          />
        </FormSection>
        <FormSection
          title="Default egress"
          description="Hosts are labeled by the access that agents may use."
        >
          <EgressFields initial={data.default_egress} />
        </FormSection>
      </PolicyForm>
      <Problem>{groups.error && runnerError(groups.error)}</Problem>
      {groups.data?.items.map((group) => (
        <FormSection
          key={group.group_id}
          id={`group-${group.group_id}`}
          title={`${group.name} runner group`}
        >
          <PolicyForm
            status={status}
            editable={editable}
            path={`/groups/${group.group_id}`}
            method="PATCH"
            body={(form) => readBudgets(form, '', caps)}
          >
            <BudgetFields keys={caps} initial={group} />
          </PolicyForm>
        </FormSection>
      ))}
    </div>
  );
}
