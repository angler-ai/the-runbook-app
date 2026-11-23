import { useState, type ReactNode } from 'react';
import { Button } from '../../ui/primitives/button';
import { runnerError } from '../runner/api';
import type { Credential } from '../runner/Credentials';
import { panel, Rows } from '../runner/presentation';
import { Problem } from '../runner/runnerForms';

type Binding = { credential_id: string; delivery: string };

export function Bindings({
  kind,
  rows,
  editable,
  initial,
  choices = [],
  save,
}: {
  kind: 'tools' | 'credentials';
  rows: { id: string; cells: ReactNode[] }[];
  editable: boolean;
  initial: unknown[];
  choices?: Credential[];
  save?: (value: Binding[]) => Promise<void>;
}) {
  const [message, setMessage] = useState('');
  const [error, setError] = useState('');
  const [pending, setPending] = useState(false);
  const [selected, setSelected] = useState(initial as Binding[]);
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
            setPending(true);
            setError('');
            setMessage('');
            try {
              await save(selected);
              setMessage('Changes saved.');
            } catch (problem) {
              setError(runnerError(problem));
            } finally {
              setPending(false);
            }
          }}
        >
          <fieldset disabled={pending} className="grid gap-3">
            <legend className="mb-2 font-medium">Shared credentials</legend>
            {choices.length ? (
              choices.map((credential) => (
                <div key={credential.id} className="grid gap-2 rounded-md border border-border p-3">
                  <p className="font-medium">{credential.name}</p>
                  {credential.delivery.map((delivery) => (
                    <label key={delivery} className="flex gap-2 text-sm capitalize">
                      <input
                        type="checkbox"
                        className="accent-accent"
                        checked={selected.some(
                          (item) =>
                            item.credential_id === credential.id && item.delivery === delivery,
                        )}
                        onChange={(event) =>
                          setSelected(
                            event.target.checked
                              ? [...selected, { credential_id: credential.id, delivery }]
                              : selected.filter(
                                  (item) =>
                                    item.credential_id !== credential.id ||
                                    item.delivery !== delivery,
                                ),
                          )
                        }
                      />
                      {delivery} delivery
                    </label>
                  ))}
                </div>
              ))
            ) : (
              <p className="text-sm text-fg-muted">No credentials are shared with this project.</p>
            )}
            <Button type="submit" className="w-fit">
              Save credentials
            </Button>
          </fieldset>
        </form>
      )}
      {message && <p role="status">{message}</p>}
      <Problem>{error}</Problem>
    </div>
  );
}
