import {
  CREDENTIAL_TYPES,
  type CredentialType,
  type CredentialTypeManifest,
} from '../../../../runner/packages/shared/src/credential-manifest';
import { useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { Button } from '../../ui/primitives/button';
import { Input, fieldClasses } from '../../ui/primitives/input';
import {
  runnerError,
  runnerRequest,
  runnerVaultIdentity,
  useRunnerMutation,
  useRunnerQuery,
  type Items,
  type Row,
  type RunnerStatus,
} from './api';
import { destinationBinding } from './destination-binding';
import { Field, list, panel, Rows, sharing } from './presentation';
import { saveCredentialSecrets, verifyCredentialReceipt, type CredentialBinding } from './seal';

export interface Credential extends CredentialBinding {
  name: string;
  creation_receipt?: string;
  status: string;
  delivery: string[];
  label: string;
  label_initialized?: boolean;
  sharing: { projects: string | string[] };
}

export function destination(credential: Credential) {
  const config = credential.config;
  return String(
    config.endpoint ?? config.url ?? (Array.isArray(config.hosts) ? config.hosts.join(', ') : '—'),
  );
}

function formDestination(fields: FormData, manifest: CredentialTypeManifest) {
  const config: Row = {};
  for (const field of manifest.configFields) {
    const value = String(fields.get(field.name) ?? '').trim();
    if (!value) continue;
    config[field.name] = field.kind.endsWith('-list')
      ? list(value)
      : field.kind === 'object'
        ? JSON.parse(value)
        : value;
  }
  const rawGroups = String(fields.get('groups') ?? '').trim();
  return { config, type: manifest.type, groups: rawGroups === 'all' ? 'all' : list(rawGroups) };
}

function bindingPreview(input?: Parameters<typeof destinationBinding>[0]) {
  try {
    return input ? destinationBinding(input) : null;
  } catch {
    return null;
  }
}

export function Credentials({
  status,
  org,
  editable,
}: {
  status: RunnerStatus;
  org: string;
  editable: boolean;
}) {
  const data = useRunnerQuery<Items<Credential>>(status, '/credentials');
  const client = useQueryClient();
  const mutation = useRunnerMutation(status);
  const [selected, select] = useState<string | null>(null);
  const [type, setType] = useState('');
  const [pending, setPending] = useState(false);
  const [preview, setPreview] = useState<Row | null>();
  const [message, setMessage] = useState('');
  const [error, setError] = useState('');
  const [confirm, setConfirm] = useState<'revoke' | 'delete'>();
  const credential = data.data?.items.find(({ id }) => id === selected);
  const chosenType = credential?.type ?? type;
  const manifest = Object.hasOwn(CREDENTIAL_TYPES, chosenType)
    ? CREDENTIAL_TYPES[chosenType as CredentialType]
    : undefined;
  const locked = !editable || status.status !== 'online' || pending || mutation.isPending;
  const displayedBinding = preview === undefined ? bindingPreview(credential) : preview;
  const secretEntry = editable && credential?.status !== 'revoked';
  function choose(id: string | null) {
    setPreview(undefined);
    select(id);
    setType('');
    setMessage('');
    setError('');
    setConfirm(undefined);
  }
  return (
    <div className="grid gap-4">
      {data.error && (
        <p role="alert" className="text-danger">
          {runnerError(data.error)}
        </p>
      )}
      {editable && (
        <Button className="w-fit" disabled={locked} onClick={() => choose('new')}>
          Add credential
        </Button>
      )}
      <Rows
        label="Credentials"
        columns={['Name', 'Type', 'Destination', 'Delivery', 'Projects', 'Status', 'Fingerprint']}
        rows={(data.data?.items ?? []).map((row) => ({
          id: row.id,
          cells: [
            <Button variant="link" size="sm" disabled={pending} onClick={() => choose(row.id)}>
              {row.name}
            </Button>,
            row.type,
            destination(row),
            row.delivery.join(', '),
            String(row.sharing.projects),
            row.status.replaceAll('_', ' '),
            Object.values(row.secret_meta)
              .map(({ fingerprint }) => fingerprint)
              .filter(Boolean)
              .join(', ') || '—',
          ],
        }))}
      />
      {selected && (
        <section className={panel}>
          <h2 className="mb-3 font-semibold">{credential?.name ?? 'Add credential'}</h2>
          {selected === 'new' && (
            <Field label="Credential type">
              <select
                className={fieldClasses + ' h-10'}
                value={type}
                disabled={locked}
                onChange={(event) => {
                  setType(event.target.value);
                  setPreview(undefined);
                }}
              >
                <option value="">Choose a type</option>
                {Object.values(CREDENTIAL_TYPES).map((item) => (
                  <option key={item.type} value={item.type}>
                    {item.title}
                  </option>
                ))}
              </select>
            </Field>
          )}
          {manifest && (
            <form
              key={selected + manifest.type}
              className="mt-3"
              onChange={(event) => {
                if (event.target instanceof HTMLInputElement && event.target.type === 'password')
                  return;
                try {
                  setPreview(
                    bindingPreview(formDestination(new FormData(event.currentTarget), manifest)),
                  );
                } catch {
                  setPreview(null);
                }
              }}
              onSubmit={async (event) => {
                event.preventDefault();
                if (locked) return;
                const form = event.currentTarget;
                const fields = new FormData(form);
                let saved = credential;
                setPending(true);
                setError('');
                setMessage('');
                try {
                  const enteredDestination = formDestination(fields, manifest);
                  const label = String(fields.get('label'));
                  const input = {
                    name: String(fields.get('name')),
                    ...enteredDestination,
                    sharing: sharing(String(fields.get('sharing'))),
                    ...((!credential ||
                      credential.label_initialized === false ||
                      label !== credential.label) && { label }),
                  };
                  const values = Object.fromEntries(
                    manifest.secretFields
                      .map(({ name }) => [name, String(fields.get(`secret:${name}`) ?? '')])
                      .filter(([, value]) => value),
                  );
                  const identityKey =
                    !credential || Object.keys(values).length
                      ? await runnerVaultIdentity(org, status.install_id!)
                      : undefined;
                  const updated = await runnerRequest<Credential>(
                    status.install_id!,
                    credential ? `/credentials/${credential.id}` : '/credentials',
                    credential ? 'PATCH' : 'POST',
                    input,
                  );
                  if (credential && updated.id !== credential.id) {
                    setError(
                      'The credential ID changed while saving. Reopen the credential and try again.',
                    );
                    return;
                  }
                  if (!credential) {
                    await verifyCredentialReceipt(
                      updated,
                      { ...input, label },
                      org,
                      status.install_id!,
                      identityKey!,
                    );
                  }
                  saved = updated;
                  await saveCredentialSecrets(
                    (path, method, body) => runnerRequest(status.install_id!, path, method, body),
                    org,
                    {
                      id: credential?.id ?? saved.id,
                      secret_meta: saved.secret_meta,
                      ...enteredDestination,
                    },
                    values,
                    {
                      installId: status.install_id!,
                      identityKey,
                    },
                  );
                  setMessage('Credential saved.');
                } catch (error) {
                  setError(runnerError(error));
                } finally {
                  for (const { name } of manifest.secretFields) {
                    const input = form.elements.namedItem(`secret:${name}`);
                    if (input instanceof HTMLInputElement) input.value = '';
                  }
                  if (saved) select(saved.id);
                  await client.invalidateQueries({ queryKey: ['runner-proxy', status.install_id] });
                  setPending(false);
                }
              }}
            >
              <fieldset className="grid gap-3" disabled={locked || !secretEntry}>
                <p className="text-sm text-fg-muted">{manifest.description}</p>
                <Field label="Name">
                  <Input name="name" required defaultValue={credential?.name} />
                </Field>
                {manifest.configFields.map((field) => {
                  const value = credential?.config[field.name];
                  const initial =
                    value == null
                      ? ''
                      : Array.isArray(value)
                        ? value.join(', ')
                        : typeof value === 'object'
                          ? JSON.stringify(value)
                          : String(value);
                  return (
                    <div key={field.name} className="grid gap-1">
                      <Field label={field.label}>
                        {field.options ? (
                          <select
                            className={fieldClasses + ' h-10'}
                            name={field.name}
                            required={field.required}
                            defaultValue={initial}
                          >
                            <option value="">Choose an option</option>
                            {field.options.map((option) => (
                              <option key={option}>{option}</option>
                            ))}
                          </select>
                        ) : (
                          <Input
                            name={field.name}
                            defaultValue={initial}
                            required={field.required}
                            type={field.kind === 'url' ? 'url' : 'text'}
                          />
                        )}
                      </Field>
                      {field.help && <p className="text-xs text-fg-muted">{field.help}</p>}
                    </div>
                  );
                })}
                <Field label="Bound runner groups (all or comma-separated group IDs)">
                  <Input name="groups" defaultValue={String(credential?.groups ?? 'all')} />
                </Field>
                <Field label="Projects (none, all, or comma-separated project IDs)">
                  <Input
                    name="sharing"
                    defaultValue={String(credential?.sharing.projects ?? 'none')}
                  />
                </Field>
                <Field label="Label">
                  <select
                    className={fieldClasses + ' h-10'}
                    name="label"
                    defaultValue={credential?.label ?? manifest.defaultLabel}
                  >
                    <option>read</option>
                    <option>write</option>
                    <option>destructive</option>
                  </select>
                </Field>
                {secretEntry && displayedBinding && (
                  <section aria-label="Bound destination" className="grid gap-1">
                    <h3 className="text-sm font-medium">Bound destination</h3>
                    <pre className="overflow-auto whitespace-pre-wrap text-xs text-fg-muted">
                      {JSON.stringify(displayedBinding, null, 2)}
                    </pre>
                  </section>
                )}
                {secretEntry &&
                  manifest.secretFields.map((field) => (
                    <Field
                      key={field.name}
                      label={
                        credential ? `Replace ${field.label.toLowerCase()} (optional)` : field.label
                      }
                    >
                      <Input
                        type="password"
                        name={`secret:${field.name}`}
                        autoComplete="new-password"
                        required={!credential}
                      />
                    </Field>
                  ))}
                {secretEntry && (
                  <Button className="w-fit" type="submit">
                    {pending ? 'Saving…' : 'Save credential'}
                  </Button>
                )}
              </fieldset>
            </form>
          )}
          {credential && editable && (
            <div className="mt-4 flex flex-wrap items-center gap-2">
              <Button
                variant="secondary"
                disabled={locked || credential.status !== 'active'}
                onClick={async () => {
                  setError('');
                  setMessage('');
                  try {
                    const result = (await mutation.mutateAsync({
                      method: 'POST',
                      path: `/credentials/${credential.id}/test`,
                    })) as { ok: boolean; error_class?: string };
                    setMessage(
                      result.ok
                        ? 'Connection passed.'
                        : `Connection failed (${result.error_class ?? 'unknown error'}).`,
                    );
                  } catch (error) {
                    setError(runnerError(error));
                  }
                }}
              >
                Test connection
              </Button>
              {credential.status !== 'revoked' && (
                <Button variant="secondary" disabled={locked} onClick={() => setConfirm('revoke')}>
                  Revoke
                </Button>
              )}
              <Button variant="secondary" disabled={locked} onClick={() => setConfirm('delete')}>
                Delete
              </Button>
            </div>
          )}
          {manifest && credential && <p className="mt-2 text-sm text-fg-muted">{manifest.test}</p>}
          {confirm && credential && (
            <div
              className="mt-3 grid justify-items-start gap-2"
              role="group"
              aria-label="Confirm credential action"
            >
              <p className="text-sm">
                {confirm === 'delete'
                  ? 'Delete this credential and destroy its stored secret?'
                  : 'Revoke access to this credential?'}
              </p>
              <div className="flex gap-2">
                <Button
                  variant="destructive"
                  disabled={locked}
                  onClick={async () => {
                    setError('');
                    try {
                      await mutation.mutateAsync({
                        method: confirm === 'delete' ? 'DELETE' : 'POST',
                        path: `/credentials/${credential.id}${confirm === 'revoke' ? '/revoke' : ''}`,
                      });
                      if (confirm === 'delete') choose(null);
                      setConfirm(undefined);
                      setMessage(
                        confirm === 'delete' ? 'Credential deleted.' : 'Credential revoked.',
                      );
                    } catch (error) {
                      setError(runnerError(error));
                    }
                  }}
                >
                  Confirm {confirm}
                </Button>
                <Button variant="secondary" onClick={() => setConfirm(undefined)}>
                  Cancel
                </Button>
              </div>
            </div>
          )}
        </section>
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
