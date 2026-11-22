import { useState, type ReactNode } from 'react';
import { Button } from '../../ui/primitives/button';
import { fieldClasses } from '../../ui/primitives/input';
import { runnerError } from '../runner/api';
import { Field, panel, Rows } from '../runner/presentation';

export function Bindings({
  kind,
  rows,
  editable,
  initial,
  save,
}: {
  kind: 'tools' | 'credentials';
  rows: { id: string; cells: ReactNode[] }[];
  editable: boolean;
  initial: unknown[];
  save?: (value: unknown[]) => Promise<void>;
}) {
  const [message, setMessage] = useState('');
  const [error, setError] = useState('');
  const [pending, setPending] = useState(false);
  return (
    <div className="grid gap-3">
      <Rows
        label={`Allowed ${kind}`}
        columns={
          kind === 'tools'
            ? ['Tool', 'Label', 'Calls', 'Errors']
            : ['Credential', 'Delivery', 'Destination']
        }
        rows={rows}
      />
      {editable && (
        <form
          className={panel}
          onSubmit={async (event) => {
            event.preventDefault();
            if (pending || !save) return;
            const fields = new FormData(event.currentTarget);
            setPending(true);
            setError('');
            setMessage('');
            try {
              const value: unknown = JSON.parse(String(fields.get('bindings')));
              if (!Array.isArray(value)) throw new Error('Bindings must be a list');
              await save(value);
              setMessage('Changes saved.');
            } catch (problem) {
              setError(runnerError(problem));
            } finally {
              setPending(false);
            }
          }}
        >
          <fieldset disabled={pending} className="grid justify-items-start gap-3">
            <Field
              label={
                kind === 'tools'
                  ? 'Tool bindings (server_id and tool)'
                  : 'Credential bindings (credential_id and delivery)'
              }
            >
              <textarea
                className={fieldClasses + ' min-h-28 py-2 font-mono'}
                name="bindings"
                defaultValue={JSON.stringify(initial, null, 2)}
              />
            </Field>
            <Button type="submit">Save {kind}</Button>
          </fieldset>
        </form>
      )}
      {message && <p role="status">{message}</p>}
      {error && (
        <p role="alert" className="text-danger">
          {error}
        </p>
      )}
    </div>
  );
}
