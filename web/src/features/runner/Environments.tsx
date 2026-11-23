import { useEffect, useState } from 'react';
import { Link, useLocation, useNavigate } from '@tanstack/react-router';
import { Box, Plus, RefreshCw } from 'lucide-react';
import { CREDENTIAL_ENV } from '../../../../runner/packages/shared/src/credential-source';
import { findSecretShapedValues } from '../../../../runner/packages/shared/src/secret-shape';
import { EmptyState } from '../../ui/layout';
import { Button } from '../../ui/primitives/button';
import { Input, fieldClasses } from '../../ui/primitives/input';
import { StatusPill } from '../../ui/status/StatusPill';
import {
  runnerError,
  useRunnerMutation,
  useRunnerQuery,
  type Items,
  type RunnerStatus,
} from './api';
import type { Credential } from './Credentials';
import { Field, Rows, list } from './presentation';
import {
  BudgetFields,
  EgressFields,
  FieldError,
  FormSection,
  RunnerForm,
  ReviewChanges,
  ReviewBudgetChanges,
  increasedCaps,
  type CapChange,
  Problem,
  numberField,
  readBudgets,
  readEgress,
  readText,
  requiredText,
  selectClasses,
  textLink,
} from './runnerForms';
import type { Environment, EnvironmentInput } from './resources';

const registryPattern =
  /^(?=.{3,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z](?:[a-z0-9-]{0,61}[a-z0-9])?(?::[1-9][0-9]{0,4})?$/;
const imagePattern =
  /^([^/]+)\/([a-z0-9]+(?:[._-]+[a-z0-9]+)*(?:\/[a-z0-9]+(?:[._-]+[a-z0-9]+)*)*)(?::([A-Za-z0-9_][A-Za-z0-9_.-]{0,127})|@(sha256:[a-f0-9]{64}))?$/;
const variableName = new RegExp(CREDENTIAL_ENV.name_pattern);
const reservedNames = new Set<string>(CREDENTIAL_ENV.reserved_names);
const byteLength = (value: string) => new TextEncoder().encode(value).length;

export function readVariables(data: FormData, prefix: string, available?: Set<string>) {
  const values = data.getAll(prefix + ':value').map(String);
  if (values.length > 64)
    throw new FieldError(prefix + ':name', 'Each variable list supports up to 64 entries.');
  const entries = data.getAll(prefix + ':name').map((raw, index) => {
    const name = String(raw).trim();
    if (!variableName.test(name) || name.length > CREDENTIAL_ENV.max_name_length)
      throw new FieldError(
        prefix + ':name',
        `Variable name in row ${index + 1} must match [A-Z_][A-Z0-9_]* and contain at most 128 characters.`,
        index,
      );
    if (
      reservedNames.has(name) ||
      CREDENTIAL_ENV.reserved_prefixes.some((prefix) => name.startsWith(prefix))
    )
      throw new FieldError(
        prefix + ':name',
        `Variable name ${name} is reserved by the sandbox.`,
        index,
      );
    const value = values[index] ?? '';
    if (!available && (byteLength(value) > 4096 || value.includes('\0')))
      throw new FieldError(
        prefix + ':value',
        `Value for ${name} must be at most 4,096 bytes and contain no null characters.`,
        index,
      );
    if (available && !available.has(value))
      throw new FieldError(
        prefix + ':value',
        `Credential for ${name} must use proxy delivery.`,
        index,
      );
    if (!available && findSecretShapedValues(value).length)
      throw new FieldError(
        prefix + ':value',
        `Value for ${name} looks like a secret. Use a credential placeholder.`,
        index,
      );
    return [name, value] as const;
  });
  if (new Set(entries.map(([name]) => name)).size !== entries.length)
    throw new FieldError(prefix + ':name', 'Variable names must be unique.');
  const variables = Object.fromEntries(entries);
  if (byteLength(JSON.stringify(variables)) > 32768)
    throw new FieldError(prefix + ':name', 'The variable list must be at most 32 KiB.');
  return variables;
}

function VariableFields({
  initial = {},
  name,
  credentials,
}: {
  initial?: Record<string, string>;
  name: string;
  credentials?: Credential[];
}) {
  const [rows, setRows] = useState(() =>
    Object.entries(initial).map(([key, value], index) => ({ id: String(index), key, value })),
  );
  return (
    <div className="grid gap-2">
      {rows.map((row, index) => (
        <div key={row.id} className="flex flex-wrap items-end gap-2">
          <div className="min-w-28 flex-1">
            <Field label={`Variable name ${index + 1}`}>
              <Input name={name + ':name'} defaultValue={row.key} placeholder="EXAMPLE_VAR" />
            </Field>
          </div>
          <div className="min-w-36 flex-1">
            {credentials ? (
              <Field label={`Credential ${index + 1}`}>
                <select className={selectClasses} name={name + ':value'} defaultValue={row.value}>
                  <option value="">Choose credential</option>
                  {row.value && !credentials.some((credential) => credential.id === row.value) && (
                    <option value={row.value}>Unavailable credential</option>
                  )}
                  {credentials.map((credential) => (
                    <option key={credential.id} value={credential.id}>
                      {credential.name}
                    </option>
                  ))}
                </select>
              </Field>
            ) : (
              <Field label={`Value ${index + 1}`}>
                <Input name={name + ':value'} defaultValue={row.value} />
              </Field>
            )}
          </div>
          <Button
            variant="secondary"
            type="button"
            aria-label={`Remove ${name === 'env' ? 'variable' : 'placeholder'} ${index + 1}`}
            onClick={() => setRows(rows.filter((item) => item.id !== row.id))}
          >
            Remove
          </Button>
        </div>
      ))}
      <Button
        type="button"
        variant="secondary"
        size="sm"
        className="w-fit"
        onClick={() => setRows([...rows, { id: crypto.randomUUID(), key: '', value: '' }])}
      >
        <Plus aria-hidden /> {credentials ? 'Add placeholder' : 'Add variable'}
      </Button>
    </div>
  );
}

function RegistryEditor({ status, editable }: { status: RunnerStatus; editable: boolean }) {
  const registries = useRunnerQuery<Items<string>>(status, '/environment-registries');
  const mutation = useRunnerMutation(status);
  const [message, setMessage] = useState('');
  const [error, setError] = useState('');
  const [review, setReview] = useState<{ items: string[]; before: string[] }>();
  async function save(items: string[]) {
    try {
      await mutation.mutateAsync({
        path: '/environment-registries',
        method: 'PUT',
        body: { items },
      });
      setMessage('Registry list saved.');
    } catch (problem) {
      setError(runnerError(problem));
    } finally {
      setReview(undefined);
    }
  }
  const additions = review?.items.filter((host) => !review.before.includes(host)) ?? [];
  const removals = review?.before.filter((host) => !review.items.includes(host)) ?? [];
  return (
    <FormSection
      title="Allowed image registries"
      description="Custom images must come from a registry on this list."
    >
      {registries.error ? (
        <Problem>{runnerError(registries.error)}</Problem>
      ) : registries.data ? (
        editable ? (
          <RunnerForm
            className="grid gap-3"
            onSubmit={async (event) => {
              event.preventDefault();
              setError('');
              setMessage('');
              const items = list(
                readText(new FormData(event.currentTarget), 'registries').replaceAll('\n', ','),
              );
              if (
                items.some(
                  (item) =>
                    !registryPattern.test(item.toLowerCase()) ||
                    Number(item.split(':')[1] ?? 443) > 65535,
                )
              ) {
                throw new FieldError(
                  'registries',
                  'Registries must be exact hostnames with an optional port, without a protocol or path.',
                );
              }
              const next = [...new Set(items.map((item) => item.toLowerCase()))];
              const before = registries.data.items;
              if (next.some((host) => !before.includes(host))) setReview({ items: next, before });
              else await save(next);
            }}
          >
            {review && (
              <ReviewChanges
                title="Review registry access"
                description="This organization can use images from added registries. Image digest resolution and cluster approval still apply."
                confirm="Allow registry changes"
                close={() => setReview(undefined)}
                onConfirm={() => save(review.items)}
                pending={mutation.isPending}
              >
                <dl className="grid gap-2 text-sm">
                  <div>
                    <dt>Current hosts</dt>
                    <dd>{review.before.join(', ') || 'None'}</dd>
                  </div>
                  <div>
                    <dt>New hosts</dt>
                    <dd>{review.items.join(', ') || 'None'}</dd>
                  </div>
                </dl>
                <ul className="grid gap-1 text-sm">
                  {additions.map((host) => (
                    <li key={host}>Add {host}</li>
                  ))}
                  {removals.map((host) => (
                    <li key={host}>Remove {host}</li>
                  ))}
                </ul>
              </ReviewChanges>
            )}
            <Field label="Registry hostnames">
              <textarea
                name="registries"
                className={fieldClasses + ' min-h-20 py-2 font-mono'}
                defaultValue={registries.data.items.join('\n')}
                placeholder="registry.example.com"
              />
            </Field>
            <Button
              type="submit"
              variant="secondary"
              className="w-fit"
              disabled={mutation.isPending}
            >
              Save registries
            </Button>
            <Problem>{error}</Problem>
            {message && (
              <p role="status" className="text-sm text-success">
                {message}
              </p>
            )}
          </RunnerForm>
        ) : (
          <p className="text-sm text-fg-muted">
            {registries.data.items.join(', ') || 'No custom registries allowed.'}
          </p>
        )
      ) : null}
    </FormSection>
  );
}

export function Environments({
  status,
  editable,
  owner,
}: {
  status: RunnerStatus;
  editable: boolean;
  owner: boolean;
}) {
  const environments = useRunnerQuery<Items<Environment>>(status, '/environments');
  return (
    <div className="grid gap-5">
      {editable && (
        <div>
          <Button asChild>
            <Link to="/runner/environments/$environment" params={{ environment: 'new' }}>
              <Plus aria-hidden /> New environment
            </Link>
          </Button>
        </div>
      )}
      <Problem>{environments.error && runnerError(environments.error)}</Problem>
      {environments.data &&
        (environments.data.items.length ? (
          <Rows
            label="Environments"
            stackOnMobile
            columns={['Environment', 'Image', 'Size', 'Version']}
            rows={environments.data.items.map((item) => ({
              id: item.id,
              cells: [
                <div>
                  <Link
                    className={textLink}
                    to="/runner/environments/$environment"
                    params={{ environment: item.id }}
                  >
                    {item.name}
                  </Link>
                  {item.description && (
                    <p className="mt-1 text-xs text-fg-muted">{item.description}</p>
                  )}
                </div>,
                <div className="min-w-40 break-all">
                  {item.image ?? 'Installation default'}
                  {item.image && !item.image_approved && (
                    <div className="mt-1">
                      <StatusPill status="waiting" label="Not yet approved for this cluster" />
                    </div>
                  )}
                </div>,
                <span className="capitalize">{item.size}</span>,
                <span className="font-mono">v{item.version}</span>,
              ],
            }))}
          />
        ) : (
          <EmptyState
            icon={Box}
            title="No custom environments"
            description="Create an environment to give a group of agents the same image, tools and budget."
          />
        ))}
      <RegistryEditor status={status} editable={editable && owner} />
    </div>
  );
}

export function EnvironmentPage({
  status,
  editable,
  id,
}: {
  status: RunnerStatus;
  editable: boolean;
  id: string;
}) {
  const hash = useLocation({ select: (location) => location.hash });
  const query = useRunnerQuery<Environment>(status, `/environments/${id}`, id !== 'new');
  const registries = useRunnerQuery<Items<string>>(status, '/environment-registries');
  const credentials = useRunnerQuery<Items<Credential>>(status, '/credentials', editable);
  useEffect(() => {
    if (query.data && hash === 'environment-budgets')
      document.getElementById(hash)?.scrollIntoView?.();
  }, [hash, query.data]);
  if (query.error) return <Problem>{runnerError(query.error)}</Problem>;
  if (id !== 'new' && !query.data) return <p role="status">Loading environment…</p>;
  return (
    <EnvironmentForm
      key={id}
      environment={query.data}
      status={status}
      editable={editable}
      registries={registries.data?.items}
      credentials={credentials.data?.items.filter(
        (item) => item.delivery.includes('proxy') && item.status !== 'revoked',
      )}
    />
  );
}

function EnvironmentForm({
  status,
  editable,
  environment,
  registries,
  credentials,
}: {
  status: RunnerStatus;
  editable: boolean;
  environment?: Environment;
  registries?: string[];
  credentials?: Credential[];
}) {
  const navigate = useNavigate();
  const mutation = useRunnerMutation(status);
  const [custom, setCustom] = useState(Boolean(environment?.image));
  const [overrideEgress, setOverrideEgress] = useState(environment?.egress != null);
  const [error, setError] = useState('');
  const [message, setMessage] = useState('');
  const [deleting, setDeleting] = useState(false);
  const path = environment ? `/environments/${environment.id}` : '/environments';

  const [review, setReview] = useState<{ body: EnvironmentInput; changes: CapChange[] }>();
  const [copied, setCopied] = useState(false);
  function readEnvironment(data: FormData) {
    const image = custom ? requiredText(data, 'image', 'Image reference', 512) : null;
    if (image && image !== environment?.image) {
      const registry = imagePattern.exec(image)?.[1];
      if (!registry)
        throw new FieldError(
          'image',
          'Image reference must use registry/repository with an optional tag or SHA-256 digest.',
        );
      if (!registries?.includes(registry))
        throw new FieldError('image', 'Image reference must use a registry in the allowed list.');
    }
    const env = readVariables(data, 'env');
    const credential_env = credentials
      ? readVariables(data, 'credential', new Set(credentials.map((item) => item.id)))
      : (environment?.credential_env ?? {});
    if (Object.keys(env).some((name) => Object.hasOwn(credential_env, name)))
      throw new FieldError(
        'credential:name',
        'Variable names must be unique across variables and credential placeholders.',
      );
    const setup = String(data.get('setup') ?? '');
    if (setup.includes('\0') || byteLength(setup) > 65536 || findSecretShapedValues(setup).length)
      throw new FieldError(
        'setup',
        'Setup script must be at most 64 KiB and contain no secrets or null characters.',
      );
    const description = readText(data, 'description');
    if (byteLength(description) > 2048)
      throw new FieldError('description', 'Description must be at most 2,048 bytes.');
    const name = requiredText(data, 'name', 'Environment name');
    if (
      byteLength(name) > 128 ||
      [...name].some((ch) => ch.charCodeAt(0) < 32 || ch.charCodeAt(0) === 127)
    )
      throw new FieldError(
        'name',
        'Environment name must be at most 128 bytes and contain no control characters.',
      );
    const body: EnvironmentInput = {
      name,
      description,
      image,
      setup: setup || null,
      setup_timeout_s: numberField(data, 'setup-timeout', 'Setup timeout', {
        min: 1,
        max: 600,
        integer: true,
        required: true,
      })!,
      timeout_s: numberField(data, 'timeout', 'Sandbox timeout', {
        min: 1,
        max: 86400,
        integer: true,
        required: true,
      })!,
      size: readText(data, 'size') as EnvironmentInput['size'],
      env,
      credential_env,
      egress: overrideEgress ? readEgress(data) : null,
      budgets: readBudgets(data, '', undefined, name),
    };
    // Keep an unchanged reference pinned. Resolving a mutable tag is a separate action.
    if (environment && image === environment.image) delete body.image;
    return body;
  }

  async function save(body: EnvironmentInput) {
    setError('');
    try {
      const result = (await mutation.mutateAsync({
        path,
        method: environment ? 'PATCH' : 'POST',
        body,
      })) as Environment;
      if (!environment)
        await navigate({
          to: '/runner/environments/$environment',
          params: { environment: result.id },
          replace: true,
        });
      setMessage('Environment saved.');
    } catch (problem) {
      setError(runnerError(problem));
    } finally {
      setReview(undefined);
    }
  }

  return (
    <RunnerForm
      className="grid gap-4"
      onSubmit={async (event) => {
        event.preventDefault();
        setError('');
        setMessage('');
        const body = readEnvironment(new FormData(event.currentTarget));
        const changes = increasedCaps(
          environment?.name ?? body.name,
          environment?.budgets ?? {},
          body.budgets ?? {},
        );
        if (changes.length) setReview({ body, changes });
        else await save(body);
      }}
    >
      {review && (
        <ReviewBudgetChanges
          changes={review.changes}
          close={() => setReview(undefined)}
          onConfirm={() => save(review.body)}
          pending={mutation.isPending}
        />
      )}
      <nav aria-label="Environment sections" className="flex flex-wrap gap-3 text-sm">
        <a className={textLink} href="#environment-image">
          Image
        </a>
        <a className={textLink} href="#environment-setup">
          Setup
        </a>
        <a className={textLink} href="#environment-credentials">
          Credentials
        </a>
        <a className={textLink} href="#environment-budgets">
          Budgets
        </a>
      </nav>
      <fieldset disabled={!editable || mutation.isPending} className="grid min-w-0 gap-4">
        <FormSection title="Environment details">
          <Field label="Environment name">
            <Input name="name" defaultValue={environment?.name} />
          </Field>
          <Field label="Description">
            <Input name="description" defaultValue={environment?.description} />
          </Field>
        </FormSection>
        <FormSection
          id="environment-image"
          title="Sandbox image"
          description="Agents use the saved image digest. Updating a tag creates a new environment version."
        >
          <Field label="Image source">
            <select
              className={selectClasses}
              value={custom ? 'custom' : 'default'}
              onChange={(event) => setCustom(event.target.value === 'custom')}
            >
              <option value="default">Installation sandbox image</option>
              <option value="custom">Custom registry image</option>
            </select>
          </Field>
          {custom && (
            <>
              <Field label="Image reference">
                <Input
                  name="image"
                  defaultValue={environment?.image ?? ''}
                  placeholder={
                    registries?.[0]
                      ? registries[0] + '/image:tag'
                      : 'registry.example.com/image:tag'
                  }
                />
              </Field>
              <p className="text-xs text-fg-muted">
                Allowed registries {registries?.join(', ') || 'none'}.{' '}
                <Link to="/runner/environments" className={textLink}>
                  Registry settings
                </Link>
              </p>
            </>
          )}
          {environment?.image_digest && (
            <div className="grid gap-2 rounded-md border border-border bg-bg p-3">
              <span className="text-xs font-medium text-fg-muted">Pinned digest</span>
              <code className="text-xs break-all">{environment.image_digest}</code>
              {environment.image && !environment.image_approved && (
                <div>
                  <StatusPill status="waiting" label="Not yet approved for this cluster" />
                  <p className="my-2 text-xs text-fg-muted">
                    Ask your cluster operator to approve this digest.
                  </p>
                  <Button
                    type="button"
                    variant="secondary"
                    size="sm"
                    onClick={async () => {
                      try {
                        await navigator.clipboard.writeText(environment.image_digest!);
                        setCopied(true);
                      } catch {
                        setError('Select the pinned digest to copy it manually.');
                      }
                    }}
                  >
                    Copy digest for cluster operator
                  </Button>
                  {copied && (
                    <span role="status" className="ml-2 text-xs">
                      Copied
                    </span>
                  )}
                </div>
              )}
              {editable && environment.image && (
                <Button
                  className="w-fit"
                  variant="secondary"
                  type="button"
                  onClick={async () => {
                    setError('');
                    try {
                      await mutation.mutateAsync({
                        path: path + '/resolve-digest',
                        method: 'POST',
                      });
                    } catch (problem) {
                      setError(runnerError(problem));
                    }
                  }}
                >
                  <RefreshCw aria-hidden /> Update to latest
                </Button>
              )}
            </div>
          )}
        </FormSection>
        <FormSection
          id="environment-setup"
          title="Sandbox setup"
          description="The script runs as the sandbox user before the first agent command."
        >
          <Field label="Setup script">
            <textarea
              name="setup"
              className={fieldClasses + ' min-h-36 py-2 font-mono'}
              spellCheck={false}
              defaultValue={environment?.setup ?? ''}
              placeholder="#!/bin/sh"
            />
          </Field>
          <Field label="Setup timeout (seconds)">
            <Input
              inputMode="numeric"
              name="setup-timeout"
              defaultValue={environment?.setup_timeout_s ?? 120}
            />
          </Field>
        </FormSection>
        <FormSection
          title="Environment variables"
          description="Use variables for non-secret configuration."
        >
          <VariableFields name="env" initial={environment?.env} />
        </FormSection>
        <FormSection
          id="environment-credentials"
          title="Credential placeholders"
          description="The CLI receives a placeholder. The proxy supplies the real credential only for its bound hosts."
        >
          <p className="text-xs text-fg-muted">
            For example, map <code>GH_TOKEN</code> to a GitHub credential bound to{' '}
            <code>api.github.com</code> before running <code>gh repo view</code>.
          </p>
          {credentials ? (
            <VariableFields
              name="credential"
              initial={environment?.credential_env}
              credentials={credentials}
            />
          ) : (
            <p className="text-sm text-fg-muted">
              {Object.keys(environment?.credential_env ?? {}).join(', ') ||
                'No credential placeholders available.'}
            </p>
          )}
        </FormSection>
        <FormSection
          title="Agent defaults"
          description="Agents can narrow these sandbox limits and host permissions."
        >
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label="Sandbox size">
              <select
                name="size"
                className={selectClasses}
                defaultValue={environment?.size ?? 'small'}
              >
                <option value="small">Small</option>
                <option value="medium">Medium</option>
                <option value="large">Large</option>
              </select>
            </Field>
            <Field label="Sandbox timeout (seconds)">
              <Input
                inputMode="numeric"
                name="timeout"
                defaultValue={environment?.timeout_s ?? 600}
              />
            </Field>
          </div>
          <label className="flex gap-2 text-sm">
            <input
              type="checkbox"
              checked={overrideEgress}
              onChange={(event) => setOverrideEgress(event.target.checked)}
              className="accent-accent"
            />{' '}
            Set an explicit host allow-list
          </label>
          {overrideEgress && <EgressFields initial={environment?.egress ?? []} />}
        </FormSection>
        <FormSection
          id="environment-budgets"
          title="Environment budgets"
          description="Blank caps inherit the applicable limits from other scopes."
        >
          <BudgetFields initial={environment?.budgets} />
        </FormSection>
      </fieldset>
      {editable && (
        <div className="sticky bottom-0 z-10 flex flex-wrap gap-2 border-t border-border bg-bg py-2">
          <Button type="submit" disabled={mutation.isPending}>
            {environment ? 'Save environment' : 'Create environment'}
          </Button>
          {environment && !environment.is_default && (
            <Button
              type="button"
              variant={deleting ? 'destructive' : 'secondary'}
              onClick={async () => {
                if (!deleting) {
                  setDeleting(true);
                  return;
                }
                setError('');
                try {
                  await mutation.mutateAsync({ path, method: 'DELETE' });
                  await navigate({ to: '/runner/environments' });
                } catch (problem) {
                  setError(runnerError(problem));
                }
              }}
            >
              {deleting ? 'Confirm delete' : 'Delete environment'}
            </Button>
          )}
          {deleting && (
            <Button type="button" variant="secondary" onClick={() => setDeleting(false)}>
              Cancel
            </Button>
          )}
        </div>
      )}
      <Problem>{error}</Problem>
      {message && (
        <p role="status" className="text-sm text-success">
          {message}
        </p>
      )}
    </RunnerForm>
  );
}
