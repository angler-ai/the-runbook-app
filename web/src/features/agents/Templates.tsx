import { useState } from 'react';
import { Button } from '../../ui/primitives/button';
import { Input, fieldClasses } from '../../ui/primitives/input';
import { runnerError, type Row } from '../runner/api';
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
  create,
  disabled,
}: {
  groups: { id: string; name: string }[];
  profiles: { id: string; name: string }[];
  create: (input: Row) => Promise<void>;
  disabled?: boolean;
}) {
  const [selected, select] = useState<keyof typeof templates | ''>('');
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  const template = selected ? templates[selected] : undefined;
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
          await create({
            name: fields.get('name'),
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
          <Input required name="name" pattern="[a-z][a-z0-9_\-]*" maxLength={128} />
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
