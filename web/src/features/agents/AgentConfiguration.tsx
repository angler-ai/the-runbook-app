import { useId, useState } from 'react';
import { Button } from '../../ui/primitives/button';
import { Input, fieldClasses } from '../../ui/primitives/input';
import { runnerError, type Row } from '../runner/api';
import { Field, list, panel } from '../runner/presentation';

export type Agent = {
  id: string;
  name: string;
  description: string;
  status: string;
  group_id: string;
  config?:
    | (Row & {
        instructions?: string;
        skills?: string[];
        profiles?: string[];
        mcp?: { server_id: string; tool: string }[];
        credentials?: { credential_id: string; delivery: string }[];
      })
    | null;
  ceiling?: {
    tools?: { server_id: string; name: string; label: string }[];
    credentials?: { id: string; name: string }[];
  } | null;
};

export function AgentConfiguration({
  agent,
  canEdit,
  admin,
  groups,
  profiles,
  save,
  operate,
}: {
  agent: Agent;
  canEdit: boolean;
  admin: boolean;
  groups: { id: string; name: string }[];
  profiles: { id: string; name: string }[];
  save: (body: Row) => Promise<void>;
  operate: (action: 'pause' | 'archive') => Promise<void>;
}) {
  const egressHint = useId();
  const [pending, setPending] = useState(false);
  const [confirmArchive, setConfirmArchive] = useState(false);
  const [feedback, setFeedback] = useState<{ error: boolean; text: string }>();
  async function perform(action: () => Promise<void>, text: string) {
    setPending(true);
    setFeedback(undefined);
    try {
      await action();
      setFeedback({ error: false, text });
    } catch (error) {
      setFeedback({ error: true, text: runnerError(error) });
    } finally {
      setPending(false);
    }
  }
  return (
    <section className={panel}>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          if (!canEdit || pending) return;
          const data = new FormData(event.currentTarget);
          void perform(async () => {
            const body: Row = {
              description: data.get('description'),
              instructions: data.get('instructions'),
              skills: list(String(data.get('skills'))),
            };
            if (admin) {
              Object.assign(body, {
                name: data.get('name'),
                status: data.get('status'),
                group_id: data.get('group_id'),
                profiles: list(String(data.get('profiles'))),
                limits: JSON.parse(String(data.get('limits'))),
                egress: JSON.parse(String(data.get('egress'))),
                sandbox: { size: data.get('sandbox') },
              });
            }
            await save(body);
          }, 'Configuration saved.');
        }}
      >
        <fieldset className="grid gap-3" disabled={pending || !canEdit}>
          <Field label="Description">
            <Input name="description" defaultValue={agent.description} />
          </Field>
          <Field label="Instructions">
            <textarea
              name="instructions"
              className={fieldClasses + ' min-h-28 py-2'}
              defaultValue={agent.config?.instructions}
            />
          </Field>
          <Field label="Skills (comma-separated)">
            <Input name="skills" defaultValue={agent.config?.skills?.join(', ')} />
          </Field>
          {admin && (
            <>
              <Field label="Agent name">
                <Input name="name" required defaultValue={agent.name} />
              </Field>
              <Field label="Status">
                <select
                  className={fieldClasses + ' h-10'}
                  name="status"
                  defaultValue={agent.status}
                >
                  {['draft', 'active', 'paused', 'archived'].map((state) => (
                    <option key={state}>{state}</option>
                  ))}
                </select>
              </Field>
              <Field label="Runner group">
                <select
                  className={fieldClasses + ' h-10'}
                  name="group_id"
                  defaultValue={agent.group_id}
                >
                  {groups.map(({ id, name }) => (
                    <option key={id} value={id}>
                      {name}
                    </option>
                  ))}
                </select>
              </Field>
              <Field label="Model profile IDs (default first, comma-separated)">
                <Input name="profiles" defaultValue={agent.config?.profiles?.join(', ')} />
              </Field>
              {profiles.length > 0 && (
                <p className="text-xs text-fg-muted">
                  {profiles.map(({ id, name }) => `${name} (${id})`).join(' · ')}
                </p>
              )}
              <Field label="Step limits (JSON)">
                <textarea
                  name="limits"
                  className={fieldClasses + ' min-h-24 py-2 font-mono'}
                  defaultValue={JSON.stringify(agent.config?.limits ?? {}, null, 2)}
                />
              </Field>
              <Field label="Egress allow-list (JSON)">
                <textarea
                  name="egress"
                  aria-describedby={egressHint}
                  className={fieldClasses + ' min-h-20 py-2 font-mono'}
                  defaultValue={JSON.stringify(agent.config?.egress ?? [], null, 2)}
                />
              </Field>
              <p id={egressHint} className="text-xs text-fg-muted">
                Sandbox HTTPS requires an allowed host and exactly one released proxy credential
                bound to it.
              </p>
              <Field label="Sandbox size">
                <select
                  className={fieldClasses + ' h-10'}
                  name="sandbox"
                  defaultValue={
                    (agent.config?.sandbox as { size?: string } | undefined)?.size ?? 'small'
                  }
                >
                  <option>small</option>
                  <option>medium</option>
                  <option>large</option>
                </select>
              </Field>
            </>
          )}
          {canEdit && (
            <div className="flex flex-wrap gap-2">
              <Button type="submit">Save configuration</Button>
              {admin && agent.status === 'active' && (
                <Button
                  variant="secondary"
                  onClick={() => void perform(() => operate('pause'), 'Agent paused.')}
                >
                  Pause agent
                </Button>
              )}
              {admin && agent.status !== 'archived' && !confirmArchive && (
                <Button variant="secondary" onClick={() => setConfirmArchive(true)}>
                  Archive agent
                </Button>
              )}
              {confirmArchive && (
                <>
                  <Button
                    variant="destructive"
                    onClick={() => void perform(() => operate('archive'), 'Agent archived.')}
                  >
                    Confirm archive
                  </Button>
                  <Button variant="secondary" onClick={() => setConfirmArchive(false)}>
                    Cancel
                  </Button>
                </>
              )}
            </div>
          )}
        </fieldset>
      </form>
      {feedback && (
        <p
          role={feedback.error ? 'alert' : 'status'}
          className={feedback.error ? 'mt-2 text-danger' : 'mt-2'}
        >
          {feedback.text}
        </p>
      )}
    </section>
  );
}
