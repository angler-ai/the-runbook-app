import { useState } from 'react';
import { Button } from '../../ui/primitives/button';
import { Input, fieldClasses } from '../../ui/primitives/input';
import { runnerError, type Row } from '../runner/api';
import { validateAgentName } from './AgentConfiguration';
import type { Credential } from '../runner/Credentials';
import type { Environment } from '../runner/resources';
import { Field, panel } from '../runner/presentation';

const templates = {
  'post-deploy': {
    name: 'Post-deploy checker',
    instructions:
      'Check the deployment health and compare error rates and latency against the baseline. Report evidence and flag issues for the operator.',
  },
  'data-qa': {
    name: 'Data QA',
    instructions:
      'Run read-only data quality checks. Report missing values, freshness, row counts, and anomalies with supporting evidence.',
  },
  'incident-triage': {
    name: 'Incident triage',
    instructions:
      'Summarize symptoms, gather recent changes and read-only diagnostics, and propose the next action for the incident lead.',
  },
} as const;

export function TemplateForm({
  groups,
  profiles,
  environments = [],
  credentials = [],
  create,
  disabled,
}: {
  groups: { id: string; name: string }[];
  profiles: { id: string; name: string }[];
  environments?: Environment[];
  credentials?: Credential[];
  create: (input: Row) => Promise<void>;
  disabled?: boolean;
}) {
  const [selected, select] = useState<keyof typeof templates | ''>('');
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  const template = selected ? templates[selected] : undefined;
  const [environmentId, setEnvironmentId] = useState('');
  const environment = environments.find((item) =>
    environmentId ? item.id === environmentId : item.is_default,
  );
  const required = [...new Set(Object.values(environment?.credential_env ?? {}))];
  const available = credentials.filter(
    (item) => required.includes(item.id) && item.delivery.includes('proxy'),
  );

  return (
    <form
      className={panel}
      onSubmit={async (event) => {
        event.preventDefault();
        if (disabled || pending) return;
        const fields = new FormData(event.currentTarget);
        setPending(true);
        setError('');
        try {
          const profile = fields.get('profile');
          const name = String(fields.get('name') ?? '').trim();
          validateAgentName(name);
          if (!fields.get('group')) throw new Error('Runner group must be selected.');
          const grants = fields.getAll('environment-credential');
          if (required.some((id) => !available.some((item) => item.id === id)))
            throw new Error(
              'Environment requires a proxy credential that is not shared with this project.',
            );
          if (required.some((id) => !grants.includes(id)))
            throw new Error('Select each required proxy credential to use this environment.');
          await create({
            name,
            ...(environmentId && { environment_id: environmentId }),
            ...(required.length && {
              credentials: required.map((credential_id) => ({ credential_id, delivery: 'proxy' })),
            }),
            group_id: fields.get('group'),
            profiles: profile ? [profile] : [],
            description: template?.name ?? '',
            instructions: fields.get('instructions'),
            status: profile ? 'active' : 'draft',
          });
        } catch (error) {
          setError(runnerError(error));
        } finally {
          setPending(false);
        }
      }}
    >
      <fieldset disabled={disabled || pending} className="grid gap-3">
        <Field label="Template">
          <select
            className={fieldClasses + ' h-10'}
            value={selected}
            onChange={(event) => select(event.target.value as typeof selected)}
          >
            <option value="">Start from scratch</option>
            {Object.entries(templates).map(([key, item]) => (
              <option key={key} value={key}>
                {item.name}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Agent name">
          <Input name="name" />
        </Field>
        <Field label="Runner group">
          <select required className={fieldClasses + ' h-10'} name="group">
            {groups.map(({ id, name }) => (
              <option key={id} value={id}>
                {name}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Model profile">
          <select className={fieldClasses + ' h-10'} name="profile">
            {profiles.map(({ id, name }) => (
              <option key={id} value={id}>
                {name}
              </option>
            ))}
            <option value="">Save as draft</option>
          </select>
        </Field>
        {environments.length > 0 && (
          <Field label="Environment">
            <select
              name="environment"
              value={environmentId}
              onChange={(event) => setEnvironmentId(event.target.value)}
              className={fieldClasses + ' h-10'}
            >
              <option value="">Organization default</option>
              {environments.map((item) => (
                <option key={item.id} value={item.id}>
                  {item.name}
                </option>
              ))}
            </select>
          </Field>
        )}
        {available.length > 0 && (
          <fieldset key={environmentId} className="grid gap-2">
            <legend className="mb-2 text-sm font-medium">Required proxy credentials</legend>
            {available.map((item) => (
              <label key={item.id} className="flex gap-2 text-sm">
                <input
                  type="checkbox"
                  name="environment-credential"
                  className="accent-accent"
                  value={item.id}
                />{' '}
                {item.name}
              </label>
            ))}
          </fieldset>
        )}
        {required.length > available.length && (
          <p role="status" className="text-xs text-warn">
            Ask a runner administrator to share the environment’s required proxy credentials with
            this project.
          </p>
        )}
        <Field label="Instructions">
          <textarea
            key={selected}
            className={fieldClasses + ' min-h-24 py-2'}
            name="instructions"
            defaultValue={template?.instructions}
          />
        </Field>
        <Button type="submit" className="w-fit" disabled={!groups.length}>
          {pending ? 'Creating…' : 'Create agent'}
        </Button>
      </fieldset>
      {error && (
        <p role="alert" className="mt-2 text-danger">
          {error}
        </p>
      )}
    </form>
  );
}
