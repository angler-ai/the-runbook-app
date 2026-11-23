import {
  CREDENTIAL_TYPES,
  type CredentialType,
} from '../../../../runner/packages/shared/src/credential-manifest';
import { useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { OAUTH_EXTRA_PARAMETERS } from '../../../../runner/packages/shared/src/oauth-binding';
import type { OAuthDiscoveryCandidate } from '../../../../runner/packages/shared/src/credential-source';
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
import { OAuthCredentialActions, oauthReturn } from './OAuthCredentialActions';
import { canonicalJson, destinationBinding } from './destination-binding';
import { Field, panel, Rows } from './presentation';
import {
  FieldError,
  RunnerForm,
  ShareFields,
  readShare,
  requiredText,
  selectClasses,
} from './runnerForms';
import { formDestination, validateSecret } from './credentialForm';
import type { Project, Share } from './resources';
import { StatusPill } from '../../ui/status/StatusPill';
import { saveCredentialSecrets, verifyCredentialReceipt, type CredentialBinding } from './seal';

export interface Credential extends CredentialBinding {
  name: string;
  creation_receipt?: string;
  status: string;
  delivery: string[];
  label: string;
  label_initialized?: boolean;
  sharing: Share;
  oauth_status?: {
    purge_until?: string;
    reconnect_reason?: string;
    refresh_failure_since?: string;
    pending_account?: string | null;
    account_confirmation_signature?: string;
    account_confirmation_expires_at_ms?: number;
    account_confirmation?: string;
    account_changed?: boolean;
    connected_account?: string;
    granted_scopes?: string[];
    grant_expires_at?: string;
    access_expires_at?: string;
    expiry_warning?: boolean;
    access_only?: boolean;
    connect_result?: string;
    discovery_candidate?: OAuthDiscoveryCandidate;
  };
}

export function destination(credential: Credential) {
  const config = credential.config;
  return String(
    config.endpoint ?? config.url ?? (Array.isArray(config.hosts) ? config.hosts.join(', ') : '—'),
  );
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
  projects = [],
  selectedCredential,
  providerKey,
}: {
  status: RunnerStatus;
  org: string;
  editable: boolean;
  projects?: Project[];
  selectedCredential?: string;
  providerKey?: { type: string; endpoint: string; onSaved(credential: Credential): void };
}) {
  const data = useRunnerQuery<Items<Credential>>(status, '/credentials');
  const client = useQueryClient();
  const mutation = useRunnerMutation(status);
  const [oauth] = useState(() => (providerKey ? undefined : oauthReturn()));
  const [selected, select] = useState<string | null>(
    providerKey ? 'new' : (selectedCredential ?? oauth?.id ?? null),
  );
  const [type, setType] = useState(providerKey ? 'model-provider' : '');
  const [sourceOverride, setSourceOverride] = useState<string>();
  const [registrationOverride, setRegistrationOverride] = useState<string>();
  const [groupsOverride, setGroupsOverride] = useState<boolean>();
  const [pending, setPending] = useState(false);
  const [preview, setPreview] = useState<Row | null>();
  const [message, setMessage] = useState(
    oauth?.result === 'connected'
      ? 'OAuth connection passed.'
      : oauth?.result === 'test-failed'
        ? 'OAuth connected. The MCP connection test failed.'
        : oauth?.result === 'failed'
          ? 'OAuth authorization did not complete.'
          : '',
  );
  const [error, setError] = useState('');
  const [confirm, setConfirm] = useState<'revoke' | 'delete'>();
  const credential = data.data?.items.find(({ id }) => id === selected);
  const chosenType = credential?.type ?? type;
  const manifest = Object.hasOwn(CREDENTIAL_TYPES, chosenType)
    ? CREDENTIAL_TYPES[chosenType as CredentialType]
    : undefined;
  const source =
    sourceOverride ?? (credential?.config.secret_source ? 'gcp-secret-manager' : 'sealed');
  const registration =
    registrationOverride ?? String(credential?.config.registration_mode ?? 'dynamic');
  const allGroups = groupsOverride ?? (!credential || credential.groups === 'all');
  const sourceConfig = credential?.config.secret_source as
    { resource?: string; version?: string } | undefined;
  const locked = !editable || status.status !== 'online' || pending || mutation.isPending;
  const displayedBinding = preview === undefined ? bindingPreview(credential) : preview;
  const oauthConfigured = credential?.type === 'mcp-oauth' && Boolean(credential.config.issuer);
  const secretFields =
    manifest?.type === 'mcp-oauth' &&
    (!oauthConfigured ||
      credential?.config.registration_mode !== 'pre-registered' ||
      credential.config.token_endpoint_auth_method === 'none')
      ? []
      : (manifest?.secretFields ?? []);
  const credentialFields =
    manifest?.configFields.filter((field) =>
      oauthConfigured
        ? ['scopes', 'extra_parameters'].includes(field.name)
        : registration !== 'dynamic' ||
          !['client_id', 'token_endpoint_auth_method'].includes(field.name),
    ) ?? [];
  const secretEntry = editable && credential?.status !== 'revoked';
  function choose(id: string | null) {
    setPreview(undefined);
    select(id);
    setType('');
    setSourceOverride(undefined);
    setRegistrationOverride(undefined);
    setGroupsOverride(undefined);
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
      {editable && !selectedCredential && !providerKey && (
        <Button className="w-fit" disabled={locked} onClick={() => choose('new')}>
          Add credential
        </Button>
      )}
      {!selectedCredential && !providerKey && (
        <Rows
          label="Credentials"
          stackOnMobile
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
              Array.isArray(row.sharing.projects)
                ? projects
                    .filter((project) => (row.sharing.projects as string[]).includes(project.id))
                    .map((project) => project.title)
                    .join(', ') || 'Selected projects'
                : row.sharing.projects === 'all'
                  ? 'All projects'
                  : 'Private',
              <StatusPill
                status={row.status === 'active' ? 'succeeded' : 'waiting'}
                label={row.status.replaceAll('_', ' ')}
                className="capitalize"
              />,
              Object.values(row.secret_meta)
                .map(({ fingerprint }) => fingerprint)
                .filter(Boolean)
                .join(', ') || '—',
            ],
          }))}
        />
      )}
      {selectedCredential && data.data && !credential && (
        <p role="status" className="text-sm text-fg-muted">
          This host connection is unavailable.
        </p>
      )}
      {selected && (!selectedCredential || credential) && (
        <section className={panel}>
          <h2 className="mb-3 font-semibold">{credential?.name ?? 'Add credential'}</h2>
          {selected === 'new' && !providerKey && (
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
            <RunnerForm
              key={selected + manifest.type}
              className="mt-3"
              noValidate
              onChange={(event) => {
                if (event.target instanceof HTMLInputElement && event.target.type === 'password')
                  return;
                try {
                  setPreview(
                    bindingPreview(
                      formDestination(
                        new FormData(event.currentTarget),
                        manifest,
                        oauthConfigured ? credential?.config : undefined,
                      ),
                    ),
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
                  const enteredDestination = formDestination(
                    fields,
                    manifest,
                    oauthConfigured ? credential?.config : undefined,
                  );
                  const label = String(fields.get('label'));
                  const input = {
                    name: requiredText(fields, 'name', 'Credential name'),
                    ...enteredDestination,
                    sharing: readShare(fields, 'sharing', credential?.sharing, projects),
                    ...((!credential ||
                      credential.label_initialized === false ||
                      label !== credential.label) && { label }),
                  };
                  const reference = enteredDestination.config.secret_source;
                  const bindingChanged = Boolean(
                    credential &&
                    canonicalJson(destinationBinding(credential)) !==
                      canonicalJson(destinationBinding(enteredDestination)),
                  );
                  const sourceChanged =
                    canonicalJson(reference ?? null) !==
                    canonicalJson(credential?.config.secret_source ?? null);
                  const values = Object.fromEntries(
                    secretFields.flatMap((field) => {
                      if (reference)
                        return bindingChanged ||
                          sourceChanged ||
                          credential?.status === 'needs_secret'
                          ? [[field.name, canonicalJson(reference)]]
                          : [];
                      const value = String(fields.get(`secret:${field.name}`) ?? '');
                      if (value || !credential || sourceChanged)
                        validateSecret(
                          value,
                          field.label,
                          manifest.type === 'mcp-bearer',
                          `secret:${field.name}`,
                        );
                      return value ? [[field.name, value]] : [];
                    }),
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
                  providerKey?.onSaved(updated);
                } catch (error) {
                  if (error instanceof FieldError) throw error;
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
                {credentialFields.map((field) => {
                  if (field.name === 'extra_parameters') {
                    const values = credential?.config.extra_parameters as
                      Record<string, string> | undefined;
                    return (
                      <fieldset
                        key={field.name}
                        className="grid gap-2 rounded-md border border-border p-3 sm:grid-cols-3"
                      >
                        <legend className="px-1 text-sm font-medium">
                          Authorization parameters (optional)
                        </legend>
                        {OAUTH_EXTRA_PARAMETERS.map((name) => (
                          <Field
                            key={name}
                            label={
                              {
                                prompt: 'Prompt',
                                access_type: 'Access type',
                                login_hint: 'Login hint',
                              }[name]
                            }
                          >
                            <Input name={name} defaultValue={values?.[name]} />
                          </Field>
                        ))}
                      </fieldset>
                    );
                  }
                  if (field.name === 'test') {
                    const value = credential?.config.test as
                      { method?: string; path?: string } | undefined;
                    return (
                      <div className="grid gap-3 sm:grid-cols-2" key={field.name}>
                        <Field label="Test method">
                          <select
                            className={selectClasses}
                            name="test-method"
                            defaultValue={value?.method ?? 'GET'}
                          >
                            <option>GET</option>
                            <option>HEAD</option>
                          </select>
                        </Field>
                        <Field label="Test path">
                          <Input name="test-path" defaultValue={value?.path ?? '/'} />
                        </Field>
                      </div>
                    );
                  }
                  if (field.name === 'registration_mode')
                    return (
                      <Field key={field.name} label={field.label}>
                        <select
                          className={selectClasses}
                          name={field.name}
                          value={registration}
                          onChange={(event) => setRegistrationOverride(event.target.value)}
                        >
                          <option value="dynamic">Dynamic registration</option>
                          <option value="pre-registered">Use an existing OAuth app</option>
                        </select>
                      </Field>
                    );
                  if (providerKey && (field.name === 'provider' || field.name === 'endpoint')) {
                    const value =
                      field.name === 'provider' ? providerKey.type : providerKey.endpoint;
                    return (
                      <div key={field.name} className="text-sm break-all">
                        <span className="font-medium">{field.label} </span>
                        {value}
                        <input type="hidden" name={field.name} value={value} />
                      </div>
                    );
                  }
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
                            inputMode={field.kind === 'url' ? 'url' : undefined}
                          />
                        )}
                      </Field>
                      {field.help && <p className="text-xs text-fg-muted">{field.help}</p>}
                    </div>
                  );
                })}
                {manifest.type === 'mcp-oauth' &&
                  !oauthConfigured &&
                  registration === 'dynamic' && (
                    <p className="text-xs text-fg-muted">
                      Client authentication is decided by registration for dynamic clients.
                    </p>
                  )}
                <fieldset className="grid gap-2 rounded-md border border-border p-3">
                  <legend className="px-1 text-sm font-medium">Bound runner groups</legend>
                  <label className="flex gap-2 text-sm">
                    <input
                      type="checkbox"
                      name="all-groups"
                      checked={allGroups}
                      onChange={(event) => setGroupsOverride(event.target.checked)}
                      className="accent-accent"
                    />
                    All runner groups
                  </label>
                  {!allGroups &&
                    status.groups.map((group) => (
                      <label className="flex gap-2 text-sm" key={group.id}>
                        <input
                          type="checkbox"
                          name="groups"
                          value={group.id}
                          className="accent-accent"
                          defaultChecked={
                            Array.isArray(credential?.groups) &&
                            credential.groups.includes(group.id)
                          }
                        />
                        {group.name}
                      </label>
                    ))}
                </fieldset>
                <ShareFields initial={credential?.sharing} projects={projects} />
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
                    <dl className="grid gap-2 rounded-md border border-border bg-bg p-3 text-sm">
                      {Object.entries(displayedBinding)
                        .filter(
                          ([key, value]) =>
                            value != null &&
                            value !== '' &&
                            !['groups', 'client_id', 'extra_parameters', 'iss_supported'].includes(
                              key,
                            ),
                        )
                        .map(([key, value]) => (
                          <div className="min-w-0" key={key}>
                            <dt className="text-xs text-fg-muted capitalize">
                              {key.replaceAll('_', ' ')}
                            </dt>
                            <dd className="break-all">
                              {value != null && typeof value === 'object' && !Array.isArray(value)
                                ? Object.entries(value)
                                    .map(
                                      ([field, value]) =>
                                        `${field.replaceAll('_', ' ')} ${String(value)}`,
                                    )
                                    .join(' · ')
                                : Array.isArray(value)
                                  ? value.join(', ') || 'None'
                                  : String(value)}
                            </dd>
                          </div>
                        ))}
                    </dl>
                  </section>
                )}
                {secretEntry && manifest.secretSources && (
                  <>
                    <Field label="Secret source">
                      <select
                        name="secret-source"
                        className={selectClasses}
                        value={source}
                        onChange={(event) => setSourceOverride(event.target.value)}
                      >
                        <option value="sealed">Enter a secret securely</option>
                        <option value="gcp-secret-manager">Stored in Google Secret Manager</option>
                      </select>
                    </Field>
                    {source === 'gcp-secret-manager' && (
                      <div className="grid gap-3 sm:grid-cols-2">
                        <Field label="Secret resource name">
                          <Input
                            name="secret-resource"
                            placeholder="projects/PROJECT/secrets/SECRET"
                            defaultValue={sourceConfig?.resource}
                          />
                        </Field>
                        <Field label="Secret version">
                          <Input
                            name="secret-version"
                            defaultValue={sourceConfig?.version ?? 'latest'}
                          />
                        </Field>
                        <p className="text-xs text-fg-muted sm:col-span-2">
                          The vault reads this version at release. “latest” picks up rotations in
                          Secret Manager. Your runner operator must allow this secret for this
                          destination and grant the vault access.
                        </p>
                      </div>
                    )}
                  </>
                )}
                {secretEntry &&
                  source !== 'gcp-secret-manager' &&
                  secretFields.map((field) => (
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
            </RunnerForm>
          )}
          {credential?.type === 'mcp-oauth' && credential.status !== 'revoked' && (
            <OAuthCredentialActions
              key={credential.id}
              credential={credential}
              install={status.install_id!}
              org={org}
              disabled={locked}
              handoff={oauth?.id === credential.id ? oauth : undefined}
              notify={setMessage}
              errors={setError}
            />
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
              {credential.status !== 'revoked' && credential.type !== 'mcp-oauth' && (
                <Button variant="secondary" disabled={locked} onClick={() => setConfirm('revoke')}>
                  Revoke
                </Button>
              )}
              <Button variant="secondary" disabled={locked} onClick={() => setConfirm('delete')}>
                Delete
              </Button>
            </div>
          )}
          {selectedCredential && (
            <a
              className="mt-3 inline-block text-sm text-accent underline"
              href="/runner/connections"
            >
              All connections
            </a>
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
