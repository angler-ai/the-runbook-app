import { useState, type ReactNode } from 'react';
import { Cpu, Plus } from 'lucide-react';
import { EmptyState } from '../../ui/layout';
import { Button } from '../../ui/primitives/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '../../ui/primitives/dialog';
import { Input } from '../../ui/primitives/input';
import { StatusPill } from '../../ui/status/StatusPill';
import {
  runnerError,
  useRunnerMutation,
  useRunnerQuery,
  type Items,
  type Row,
  type RunnerStatus,
} from './api';
import type { Credential } from './Credentials';
import { Field, Rows, panel } from './presentation';
import {
  Problem,
  ShareFields,
  endpoint,
  numberField,
  readShare,
  readText,
  requiredText,
  selectClasses,
} from './runnerForms';
import type { Named, Project, Share } from './resources';

type Price = {
  id: string;
  input_usd_per_million: number;
  output_usd_per_million: number;
  cached_usd_per_million?: number;
  cache_write_usd_per_million?: number;
};
type Provider = Named & {
  type: string;
  auth?: string;
  endpoint: string;
  credential_id?: string;
  region?: string;
  models: Price[];
  sharing: Share;
};
type Profile = Named & {
  provider_id: string;
  model: string;
  options: Row;
  sharing: Share;
  verified_at?: string | null;
};

const sonnet = {
  id: 'claude-sonnet-5-5',
  input_usd_per_million: 2,
  output_usd_per_million: 10,
  cached_usd_per_million: 0.2,
  cache_write_usd_per_million: 2.5,
};
const providerPresets = {
  anthropic: {
    name: 'Anthropic',
    endpoint: 'https://api.anthropic.com',
    models: [sonnet],
    pricing: 'https://platform.claude.com/docs/en/about-claude/pricing',
  },
  vertex: {
    name: 'Vertex Claude',
    endpoint:
      'https://aiplatform.googleapis.com/v1/projects/PROJECT/locations/global/publishers/anthropic/models',
    models: [sonnet],
    pricing: 'https://cloud.google.com/model-garden/pricing',
  },
  'vertex-gemini': {
    name: 'Vertex Gemini',
    endpoint:
      'https://aiplatform.googleapis.com/v1/projects/PROJECT/locations/global/publishers/google/models',
    models: [
      {
        id: 'gemini-4-hydrogen',
        input_usd_per_million: 1.5,
        output_usd_per_million: 7.5,
      },
    ],
    pricing: 'https://cloud.google.com/model-garden/pricing',
  },
  'openai-compatible': {
    name: 'OpenAI-compatible',
    endpoint: 'https://api.openai.com/v1',
    models: [
      {
        id: 'gpt-4o-mini',
        input_usd_per_million: 0.15,
        output_usd_per_million: 0.6,
        cached_usd_per_million: 0.075,
      },
    ],
    pricing: 'https://developers.openai.com/api/docs/pricing',
  },
};
function modelEndpoint(raw: string, type: string) {
  const value = endpoint(raw.trim(), 'Provider endpoint', { http: type === 'openai-compatible' });
  const url = new URL(value);
  if (type === 'anthropic' && url.pathname !== '/')
    throw new Error(
      'Anthropic provider endpoint must be an origin without a path. The runner adds /v1.',
    );
  if (type === 'vertex-gemini') {
    const match =
      /^\/v1\/projects\/([\w-]+)\/locations\/([\w-]+)\/publishers\/google\/models\/?$/.exec(
        url.pathname,
      );
    const location = match?.[2];
    const host =
      location === 'global'
        ? 'aiplatform.googleapis.com'
        : location === 'us' || location === 'eu'
          ? `aiplatform.${location}.rep.googleapis.com`
          : `${location}-aiplatform.googleapis.com`;
    if (!match || match[1] === 'PROJECT' || url.hostname !== host || url.port)
      throw new Error(
        'Vertex Gemini endpoint must include your Google Cloud project and location and end with /publishers/google/models.',
      );
  }
  return value;
}

const providerName = (type: string) =>
  providerPresets[type as keyof typeof providerPresets]?.name ?? type;
const priceFields = [
  ['input_usd_per_million', 'Input'],
  ['output_usd_per_million', 'Output'],
  ['cached_usd_per_million', 'Cache read'],
  ['cache_write_usd_per_million', 'Cache write'],
] as const;

export function Models({
  status,
  editable,
  projects,
}: {
  status: RunnerStatus;
  editable: boolean;
  projects: Project[];
}) {
  const providers = useRunnerQuery<Items<Provider>>(status, '/providers');
  const profiles = useRunnerQuery<Items<Profile>>(status, '/profiles');
  const credentials = useRunnerQuery<Items<Credential>>(status, '/credentials', editable);
  const [editor, setEditor] = useState<
    { kind: 'provider'; item?: Provider } | { kind: 'profile'; item?: Profile }
  >();
  const options = providers.data?.items ?? [];
  return (
    <div className="grid gap-6">
      <Problem>{providers.error && runnerError(providers.error)}</Problem>
      <section className="grid gap-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div>
            <h2 className="font-semibold">Model providers</h2>
            <p className="text-sm text-fg-muted">Endpoints and prices used by the runner.</p>
          </div>
          {editable && (
            <Button onClick={() => setEditor({ kind: 'provider' })}>
              <Plus aria-hidden /> Add provider
            </Button>
          )}
        </div>
        {options.length ? (
          <div className="grid gap-3 md:grid-cols-2">
            {options.map((provider) => (
              <article key={provider.id} className={panel + ' grid min-w-0 gap-3'}>
                <div className="flex items-start justify-between gap-2">
                  <div className="min-w-0">
                    <h3 className="font-semibold">{provider.name}</h3>
                    <p className="text-xs break-all text-fg-muted">
                      {providerName(provider.type)} · {provider.endpoint}
                    </p>
                  </div>
                  {editable && (
                    <Button
                      size="sm"
                      variant="secondary"
                      onClick={() => setEditor({ kind: 'provider', item: provider })}
                    >
                      Edit<span className="sr-only"> {provider.name}</span>
                    </Button>
                  )}
                </div>
                <Rows
                  label={provider.name + ' prices in USD per million tokens'}
                  columns={['Model', 'Input / 1M', 'Output / 1M']}
                  rows={provider.models.map((item) => ({
                    id: item.id,
                    cells: [
                      <span className="break-all">{item.id}</span>,
                      `$${item.input_usd_per_million}`,
                      `$${item.output_usd_per_million}`,
                    ],
                  }))}
                />
              </article>
            ))}
          </div>
        ) : (
          providers.data && (
            <EmptyState
              icon={Cpu}
              title="No model providers"
              description="Add the provider your runner will use for model calls."
            />
          )
        )}
      </section>
      <section className="grid gap-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div>
            <h2 className="font-semibold">Model profiles</h2>
            <p className="text-sm text-fg-muted">
              Name the model configurations available to agents.
            </p>
          </div>
          {editable && (
            <Button
              variant="secondary"
              disabled={!options.length}
              onClick={() => setEditor({ kind: 'profile' })}
            >
              <Plus aria-hidden /> Add profile
            </Button>
          )}
        </div>
        <Problem>{profiles.error && runnerError(profiles.error)}</Problem>
        <Rows
          label="Model profiles"
          stackOnMobile
          columns={['Profile', 'Provider', 'Model', 'Status']}
          rows={(profiles.data?.items ?? []).map((profile) => ({
            id: profile.id,
            cells: [
              editable ? (
                <Button
                  className="h-auto p-0"
                  variant="link"
                  onClick={() => setEditor({ kind: 'profile', item: profile })}
                >
                  {profile.name}
                </Button>
              ) : (
                profile.name
              ),
              options.find((provider) => provider.id === profile.provider_id)?.name ??
                'Unavailable provider',
              profile.model,
              <StatusPill
                status={profile.verified_at ? 'succeeded' : 'pending'}
                label={profile.verified_at ? 'Verified' : 'Unverified'}
              />,
            ],
          }))}
        />
      </section>
      <Dialog
        open={Boolean(editor)}
        onOpenChange={(open) => {
          if (!open) setEditor(undefined);
        }}
      >
        {editor && (
          <DialogContent className="max-w-3xl">
            <DialogHeader>
              <DialogTitle>
                {editor.item ? `Edit ${editor.item.name}` : `Add model ${editor.kind}`}
              </DialogTitle>
              <DialogDescription>
                {editor.kind === 'provider'
                  ? 'Set up an endpoint and token prices for budgeting.'
                  : 'Choose a provider and model for your agents.'}
              </DialogDescription>
            </DialogHeader>
            {editor.kind === 'provider' ? (
              <ProviderForm
                initial={editor.item}
                status={status}
                projects={projects}
                credentials={credentials.data?.items ?? []}
                close={() => setEditor(undefined)}
              />
            ) : (
              <ProfileForm
                initial={editor.item}
                status={status}
                projects={projects}
                providers={options}
                close={() => setEditor(undefined)}
              />
            )}
          </DialogContent>
        )}
      </Dialog>
    </div>
  );
}

function ModelForm({
  status,
  path,
  close,
  children,
  save,
  existing,
  unavailable,
}: {
  status: RunnerStatus;
  path: string;
  close(): void;
  children: ReactNode;
  save(data: FormData): Row;
  existing: boolean;
  unavailable?: string;
}) {
  const mutation = useRunnerMutation(status);
  const [error, setError] = useState('');
  const [deleting, setDeleting] = useState(false);
  async function perform(method: 'POST' | 'PATCH' | 'DELETE', data?: FormData) {
    setError('');
    try {
      await mutation.mutateAsync({ path, method, body: data && save(data) });
      close();
    } catch (problem) {
      setError(runnerError(problem));
    }
  }
  return (
    <form
      className="grid gap-4"
      onSubmit={(event) => {
        event.preventDefault();
        void perform(existing ? 'PATCH' : 'POST', new FormData(event.currentTarget));
      }}
    >
      <fieldset className="grid min-w-0 gap-4" disabled={mutation.isPending}>
        {children}
      </fieldset>
      <Problem>{error || unavailable}</Problem>
      <div className="flex flex-wrap gap-2">
        <Button type="submit" disabled={mutation.isPending || Boolean(unavailable)}>
          Save changes
        </Button>
        {existing && (
          <Button
            type="button"
            variant={deleting ? 'destructive' : 'secondary'}
            disabled={mutation.isPending}
            onClick={() => (deleting ? void perform('DELETE') : setDeleting(true))}
          >
            {deleting ? 'Confirm delete' : 'Delete'}
          </Button>
        )}
        <Button type="button" variant="secondary" onClick={close}>
          Cancel
        </Button>
      </div>
    </form>
  );
}

function ProviderForm({
  initial,
  status,
  projects,
  credentials,
  close,
}: {
  initial?: Provider;
  status: RunnerStatus;
  projects: Project[];
  credentials: Credential[];
  close(): void;
}) {
  const [type, setType] = useState(initial?.type ?? 'anthropic');
  const preset =
    providerPresets[type as keyof typeof providerPresets] ?? providerPresets['openai-compatible'];
  const [url, setUrl] = useState(initial?.endpoint ?? preset.endpoint);
  const [prices, setPrices] = useState<{ key: string; value: Price }[]>(() =>
    (initial?.models ?? preset.models).map((value, index) => ({ key: String(index), value })),
  );
  const matching = credentials.filter(
    (item) =>
      item.type === 'model-provider' &&
      item.status !== 'revoked' &&
      item.config.provider === type &&
      String(item.config.endpoint).replace(/\/+$/, '') === url.trim().replace(/\/+$/, ''),
  );
  return (
    <ModelForm
      status={status}
      close={close}
      existing={Boolean(initial)}
      path={initial ? `/providers/${initial.id}` : '/providers'}
      unavailable={
        type === 'vertex'
          ? 'Vertex Claude requires attached identity. The runner currently disables this provider in its catalog.'
          : undefined
      }
      save={(data) => {
        const models = prices.map(({ key }, index) => {
          const id = requiredText(data, key + ':model', `Model name in row ${index + 1}`);
          return {
            id,
            ...Object.fromEntries(
              priceFields.flatMap(([field, label], column) => {
                const value = numberField(data, key + ':' + field, `${label} price for ${id}`, {
                  required: column < 2,
                });
                return value == null ? [] : [[field, value]];
              }),
            ),
          };
        });
        if (!models.length)
          throw new Error('Add at least one model and its input and output prices.');
        if (new Set(models.map((model) => model.id)).size !== models.length)
          throw new Error('Model names must be unique within a provider.');
        const providerEndpoint = modelEndpoint(url, type);
        return {
          name: requiredText(data, 'name', 'Provider name'),
          type,
          endpoint: providerEndpoint,
          credential_id: requiredText(
            data,
            'credential',
            'A credential bound to this provider and endpoint',
          ),
          auth: 'stored',
          models,
          sharing: readShare(data, 'sharing', initial?.sharing, projects),
        };
      }}
    >
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label="Provider preset">
          <select
            value={type}
            className={selectClasses}
            onChange={(event) => {
              const next = event.target.value as keyof typeof providerPresets;
              const selected = providerPresets[next];
              setType(next);
              setUrl(selected.endpoint);
              setPrices(selected.models.map((value) => ({ key: crypto.randomUUID(), value })));
            }}
          >
            {Object.entries(providerPresets).map(([key, value]) => (
              <option key={key} value={key}>
                {value.name}
              </option>
            ))}
            {!Object.hasOwn(providerPresets, type) && <option value={type}>{type}</option>}
          </select>
        </Field>
        <Field label="Provider name">
          <Input name="name" placeholder={preset.name} defaultValue={initial?.name} />
        </Field>
        <Field label="Provider endpoint">
          <Input inputMode="url" value={url} onChange={(event) => setUrl(event.target.value)} />
        </Field>
        <Field label="Model credential">
          <select name="credential" className={selectClasses} defaultValue={initial?.credential_id}>
            <option value="">Choose a bound credential</option>
            {matching.map((item) => (
              <option key={item.id} value={item.id}>
                {item.name}
              </option>
            ))}
          </select>
        </Field>
      </div>
      {type.startsWith('vertex') && (
        <p className="text-xs text-fg-muted">
          Replace PROJECT in the endpoint with your Google Cloud project. The endpoint and
          credential must match.
        </p>
      )}
      {!matching.length && type !== 'vertex' && (
        <p className="text-sm text-fg-muted">
          Add a model provider key in{' '}
          <a className="text-accent underline" href="/runner/credentials">
            Credentials
          </a>{' '}
          for this endpoint.
        </p>
      )}
      <div className="grid gap-2">
        <h3 className="font-medium">Prices per million tokens (USD)</h3>
        {'pricing' in preset && (
          <p className="text-xs text-fg-muted">
            Confirm preset prices for your account and region.{' '}
            <a className="text-accent underline" href={preset.pricing}>
              Provider pricing
            </a>
          </p>
        )}
        {prices.map(({ key, value }, index) => (
          <div
            className="grid grid-cols-2 gap-2 rounded-md border border-border p-2 sm:grid-cols-4"
            key={key}
          >
            <div className="col-span-2 sm:col-span-3">
              <Field label={`Model ${index + 1}`}>
                <Input name={key + ':model'} defaultValue={value.id} />
              </Field>
            </div>
            <Button
              type="button"
              variant="secondary"
              className="self-end"
              onClick={() => setPrices(prices.filter((row) => row.key !== key))}
            >
              Remove<span className="sr-only"> model {index + 1}</span>
            </Button>
            {priceFields.map(([field, label]) => (
              <Field key={field} label={label}>
                <Input inputMode="decimal" name={key + ':' + field} defaultValue={value[field]} />
              </Field>
            ))}
          </div>
        ))}
        <Button
          className="w-fit"
          variant="secondary"
          type="button"
          onClick={() =>
            setPrices([
              ...prices,
              {
                key: crypto.randomUUID(),
                value: { id: '', input_usd_per_million: 0, output_usd_per_million: 0 },
              },
            ])
          }
        >
          <Plus aria-hidden /> Add model
        </Button>
      </div>
      <ShareFields projects={projects} initial={initial?.sharing} />
    </ModelForm>
  );
}

function ProfileForm({
  initial,
  status,
  projects,
  providers,
  close,
}: {
  initial?: Profile;
  status: RunnerStatus;
  projects: Project[];
  providers: Provider[];
  close(): void;
}) {
  const [providerId, setProvider] = useState(initial?.provider_id ?? '');
  const provider = providers.find((item) => item.id === providerId);
  return (
    <ModelForm
      status={status}
      close={close}
      existing={Boolean(initial)}
      path={initial ? `/profiles/${initial.id}` : '/profiles'}
      save={(data) => {
        const name = requiredText(data, 'name', 'Profile name');
        const model = requiredText(data, 'model', 'Model');
        if (!provider?.models.some((item) => item.id === model))
          throw new Error('Model must belong to the selected provider.');
        const {
          effort: _effort,
          max_output_tokens: _tokens,
          max_response_bytes: _bytes,
          ...options
        } = initial?.options ?? {};
        const effort = readText(data, 'effort');
        const tokens = numberField(data, 'tokens', 'Maximum output tokens', {
          min: 1,
          max: 2_147_483_647,
          integer: true,
        });
        const bytes = numberField(data, 'bytes', 'Maximum response bytes', {
          min: 1,
          max: 2_147_483_647,
          integer: true,
        });
        return {
          name,
          provider_id: provider.id,
          model,
          options: {
            ...options,
            ...(effort && { effort }),
            ...(tokens && { max_output_tokens: tokens }),
            ...(bytes && { max_response_bytes: bytes }),
          },
          sharing: readShare(data, 'sharing', initial?.sharing, projects),
        };
      }}
    >
      <Field label="Profile name">
        <Input name="name" defaultValue={initial?.name} placeholder="fast" />
      </Field>
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label="Provider">
          <select
            className={selectClasses}
            value={providerId}
            onChange={(event) => setProvider(event.target.value)}
          >
            <option value="">Choose provider</option>
            {providers.map((item) => (
              <option key={item.id} value={item.id}>
                {item.name}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Model">
          <select
            key={providerId}
            name="model"
            className={selectClasses}
            defaultValue={initial?.model ?? ''}
          >
            <option value="">Choose model</option>
            {provider?.models.map((item) => (
              <option key={item.id} value={item.id}>
                {item.id}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Reasoning effort">
          <select
            name="effort"
            className={selectClasses}
            defaultValue={String(initial?.options?.effort ?? '')}
          >
            <option value="">Provider default</option>
            <option value="low">Low</option>
            <option value="medium">Medium</option>
            <option value="high">High</option>
          </select>
        </Field>
        <Field label="Maximum output tokens">
          <Input
            name="tokens"
            inputMode="numeric"
            defaultValue={String(initial?.options?.max_output_tokens ?? '')}
          />
        </Field>
        <Field label="Maximum response bytes">
          <Input
            name="bytes"
            inputMode="numeric"
            defaultValue={String(initial?.options?.max_response_bytes ?? '')}
          />
        </Field>
      </div>
      <ShareFields projects={projects} initial={initial?.sharing} />
    </ModelForm>
  );
}
