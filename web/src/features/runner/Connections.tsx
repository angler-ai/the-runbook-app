import { useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { Link, useNavigate } from '@tanstack/react-router';
import {
  ArrowRight,
  BookOpen,
  Cable,
  CircleCheck,
  GitBranch,
  Globe,
  Layers3,
  ListTodo,
  Megaphone,
  Plus,
  ShieldCheck,
} from 'lucide-react';
import { EmptyState } from '../../ui/layout';
import { Button } from '../../ui/primitives/button';
import { Input } from '../../ui/primitives/input';
import { StatusPill } from '../../ui/status/StatusPill';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '../../ui/primitives/tabs';
import {
  runnerError,
  runnerRequest,
  runnerVaultIdentity,
  useRunnerMutation,
  useRunnerQuery,
  type Items,
  type RunnerStatus,
} from './api';
import { ConnectionTools } from './ConnectionTools';
import { Credentials, destination, type Credential } from './Credentials';
import { OAuthCredentialActions } from './OAuthCredentialActions';
import { Field, panel, sentenceCase } from './presentation';
import {
  FormSection,
  Problem,
  ShareFields,
  endpoint,
  readShare,
  readText,
  requiredText,
  selectClasses,
  textLink,
} from './runnerForms';
import { validateSecret } from './credentialForm';
import { saveCredentialSecrets, verifyCredentialReceipt } from './seal';
import type { Connection, Project } from './resources';

const presets = [
  {
    name: 'Notion',
    url: 'https://mcp.notion.com/mcp',
    auth: 'oauth',
    detail: 'Workspace pages and databases',
    icon: BookOpen,
  },
  {
    name: 'Meta Ads',
    url: 'https://mcp.facebook.com/ads',
    auth: 'bearer',
    detail: 'Ad accounts and campaign data',
    icon: Megaphone,
  },
  { name: 'Angler MCP', url: '', auth: 'oauth', detail: 'Your Angler deployment', icon: Layers3 },
  {
    name: 'GitHub',
    url: 'https://api.githubcopilot.com/mcp/readonly',
    auth: 'bearer',
    detail: 'Repositories, issues and pull requests',
    icon: GitBranch,
  },
  {
    name: 'Linear',
    url: 'https://mcp.linear.app/mcp/readonly',
    auth: 'bearer',
    detail: 'Issues and project tracking',
    icon: ListTodo,
  },
  { name: 'Custom', url: '', auth: 'none', detail: 'An MCP server you manage', icon: Cable },
] as const;

type Auth = 'oauth' | 'bearer' | 'none';
const wizardKey = (credential: string) => `runner-connect:${credential}`;
function activeConnection(credential: string) {
  try {
    return sessionStorage.getItem(wizardKey(credential));
  } catch {
    return null;
  }
}
function rememberConnection(id: string, connection: string | null) {
  try {
    if (connection) sessionStorage.setItem(wizardKey(id), connection);
    else sessionStorage.removeItem(wizardKey(id));
  } catch {
    /* Storage may be disabled. Connection management remains available. */
  }
}

function ConnectionProgress({ step }: { step: number }) {
  const steps = ['System', 'Endpoints', 'Sign-in', 'Tools', 'Sharing'];
  return (
    <ol
      aria-label="Connection progress"
      className="grid grid-cols-5 gap-1 rounded-lg border border-border bg-surface p-2 sm:gap-2"
    >
      {steps.map((label, index) => (
        <li
          key={label}
          aria-current={step === index ? 'step' : undefined}
          className={`flex min-w-0 flex-col items-center gap-1.5 rounded-md px-1 py-2 text-center text-xs sm:flex-row sm:gap-2 sm:px-2 ${step === index ? 'bg-accent-subtle font-medium text-accent' : 'text-fg-muted'}`}
        >
          {index < step ? (
            <CircleCheck className="size-4 shrink-0 text-accent" aria-hidden />
          ) : (
            <span
              className={`grid size-5 shrink-0 place-items-center rounded-full border font-mono text-[10px] ${index === step ? 'border-accent' : 'border-border'}`}
              aria-hidden
            >
              {index + 1}
            </span>
          )}
          {label}
        </li>
      ))}
    </ol>
  );
}

function connectionStatus(credential: Credential | undefined, id?: string | null) {
  if (!credential) return id ? 'Credential unavailable' : 'No sign-in needed';
  if (credential.status === 'active')
    return credential.oauth_status?.connected_account
      ? `Connected as ${credential.oauth_status.connected_account}`
      : 'Connected';
  if (credential.status === 'reconnect_required') return 'Reconnect required';
  return sentenceCase(credential.status);
}

function ConnectionStatusPill({ credential, id }: { credential?: Credential; id?: string | null }) {
  return (
    <StatusPill
      status={!id ? 'skipped' : credential?.status === 'active' ? 'succeeded' : 'waiting'}
      label={credential?.status === 'active' ? 'Connected' : connectionStatus(credential, id)}
    />
  );
}

export function shareLabel(connection: Connection, projects: Project[]) {
  const scope = connection.sharing.projects;
  return scope === 'all'
    ? 'All projects'
    : Array.isArray(scope) && scope.length
      ? projects
          .filter((project) => scope.includes(project.id))
          .map((project) => project.title)
          .join(', ') || 'Selected projects'
      : 'Private';
}

function ConnectionCard({
  connection,
  credential,
  projects,
}: {
  connection: Connection;
  credential?: Credential;
  projects: Project[];
}) {
  const Icon =
    presets.find((item) => item.url && connection.url.startsWith(item.url))?.icon ?? Cable;
  const auth =
    credential?.type === 'mcp-oauth'
      ? 'OAuth'
      : credential?.type === 'api-key'
        ? 'API key'
        : connection.credential_id
          ? 'Bearer token'
          : 'None';
  return (
    <Link
      to="/runner/connections/$connection"
      params={{ connection: connection.id }}
      className={
        panel +
        ' group grid gap-4 transition-colors hover:border-border-strong hover:bg-surface-raised'
      }
    >
      <div className="flex min-w-0 items-start gap-3">
        <span className="grid size-10 shrink-0 place-items-center rounded-lg border border-border bg-bg text-accent">
          <Icon className="size-5" aria-hidden />
        </span>
        <div className="min-w-0 flex-1">
          <h2 className="truncate font-semibold">{connection.name}</h2>
          <p className="truncate text-xs text-fg-muted">{connection.url}</p>
        </div>
        <ArrowRight
          className="size-4 shrink-0 text-fg-muted transition-transform group-hover:translate-x-0.5"
          aria-hidden
        />
      </div>
      <div className="grid justify-items-start gap-2">
        <ConnectionStatusPill credential={credential} id={connection.credential_id} />
        {credential?.oauth_status?.connected_account && (
          <p className="text-sm break-words">{connectionStatus(credential)}</p>
        )}
        {credential?.oauth_status?.grant_expires_at && (
          <p className="text-xs text-fg-muted">
            Expires {new Date(credential.oauth_status.grant_expires_at).toLocaleDateString()}
          </p>
        )}
      </div>
      <dl className="grid grid-cols-2 gap-x-3 gap-y-2 border-t border-border pt-3 text-xs">
        <div>
          <dt className="text-fg-muted">Sign-in</dt>
          <dd className="mt-0.5 font-medium">{auth}</dd>
        </div>
        <div>
          <dt className="text-fg-muted">Tools</dt>
          <dd className="mt-0.5 font-medium">
            {connection.pinned_snapshot_id
              ? (connection.pinned_tool_count ?? 'Pinned')
              : 'Not pinned'}
          </dd>
        </div>
        <div>
          <dt className="text-fg-muted">Label</dt>
          <dd className="mt-0.5 font-medium">{sentenceCase(credential?.label ?? 'Per tool')}</dd>
        </div>
        <div>
          <dt className="text-fg-muted">Shared with</dt>
          <dd className="mt-0.5 font-medium">{shareLabel(connection, projects)}</dd>
        </div>
      </dl>
    </Link>
  );
}

function HostConnectionCard({
  credential,
  projects,
}: {
  credential: Credential;
  projects: Project[];
}) {
  const hostShare = credential.sharing.projects;
  const shared =
    hostShare === 'all'
      ? 'All projects'
      : Array.isArray(hostShare) && hostShare.length
        ? projects
            .filter((project) => hostShare.includes(project.id))
            .map((project) => project.title)
            .join(', ') || 'Selected projects'
        : 'Private';
  return (
    <Link
      to="/runner/connections/$connection"
      params={{ connection: `host-${credential.id}` }}
      className={panel + ' group grid gap-4 transition-colors hover:bg-surface-raised'}
    >
      <div className="flex min-w-0 items-start gap-3">
        <span className="grid size-10 shrink-0 place-items-center rounded-lg border border-border bg-bg text-accent">
          <ShieldCheck className="size-5" aria-hidden />
        </span>
        <div className="min-w-0 flex-1">
          <h2 className="truncate font-semibold">{credential.name}</h2>
          <p className="truncate text-xs text-fg-muted">{destination(credential)}</p>
        </div>
        <ArrowRight
          className="size-4 shrink-0 text-fg-muted transition-transform group-hover:translate-x-0.5"
          aria-hidden
        />
      </div>
      <div>
        <StatusPill
          status={credential.status === 'active' ? 'succeeded' : 'waiting'}
          label={connectionStatus(credential)}
        />
      </div>
      <dl className="grid grid-cols-2 gap-x-3 gap-y-2 border-t border-border pt-3 text-xs">
        <div>
          <dt className="text-fg-muted">Sign-in</dt>
          <dd className="mt-0.5 font-medium">API key</dd>
        </div>
        <div>
          <dt className="text-fg-muted">Delivery</dt>
          <dd className="mt-0.5 font-medium">Proxy</dd>
        </div>
        <div>
          <dt className="text-fg-muted">Label</dt>
          <dd className="mt-0.5 font-medium">{sentenceCase(credential.label)}</dd>
        </div>
        <div>
          <dt className="text-fg-muted">Shared with</dt>
          <dd className="mt-0.5 font-medium">{shared}</dd>
        </div>
      </dl>
    </Link>
  );
}

type Props = { status: RunnerStatus; org: string; editable: boolean; projects: Project[] };

export function Connections({ status, editable, projects }: Props) {
  const query = useRunnerQuery<Items<Connection>>(status, '/mcp-servers');
  const credentials = useRunnerQuery<Items<Credential>>(status, '/credentials');
  const hosts = credentials.data?.items.filter((item) => item.type === 'api-key') ?? [];
  const connections = query.data?.items ?? [];
  const action = (
    <Button asChild>
      <Link to="/runner/connections/$connection" params={{ connection: 'new' }}>
        <Plus aria-hidden /> Connect a system
      </Link>
    </Button>
  );
  return (
    <div className="grid gap-4">
      <Problem>{query.error && runnerError(query.error)}</Problem>
      {query.isPending ? (
        <p role="status">Loading connections…</p>
      ) : connections.length || hosts.length ? (
        <>
          {editable && <div className="flex sm:justify-end">{action}</div>}
          <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
            {connections.map((connection) => (
              <ConnectionCard
                key={connection.id}
                connection={connection}
                projects={projects}
                credential={credentials.data?.items.find(
                  (item) => item.id === connection.credential_id,
                )}
              />
            ))}
            {hosts.map((credential) => (
              <HostConnectionCard key={credential.id} credential={credential} projects={projects} />
            ))}
          </div>
        </>
      ) : (
        query.data && (
          <EmptyState
            icon={Cable}
            title="No connected systems"
            description="Connect an MCP server to review its tools and share them with your projects."
            action={editable ? action : undefined}
          />
        )
      )}
    </div>
  );
}

export function ConnectionPage({ id, tab, ...props }: Props & { id: string; tab?: string }) {
  const hostId = id.startsWith('host-') ? id.slice(5) : undefined;
  const query = useRunnerQuery<Connection>(
    props.status,
    `/mcp-servers/${id}`,
    id !== 'new' && !hostId,
  );
  const credential = useRunnerQuery<Credential>(
    props.status,
    `/credentials/${query.data?.credential_id ?? '-'}`,
    Boolean(query.data?.credential_id),
  );
  if (hostId) return <Credentials {...props} selectedCredential={hostId} />;
  if (id === 'new')
    return props.editable ? (
      <StartConnection {...props} />
    ) : (
      <Problem>You need runner administration access to connect a system.</Problem>
    );
  if (query.error) return <Problem>{runnerError(query.error)}</Problem>;
  if (!query.data) return <p role="status">Loading connection…</p>;
  return (
    <ConnectionDetail
      key={id}
      connection={query.data}
      credential={credential.data}
      tab={tab}
      {...props}
    />
  );
}

function StartConnection({ status, org }: Props) {
  const [presetIndex, setPresetIndex] = useState(0);
  const [auth, setAuth] = useState<Auth>('oauth');
  const [registration, setRegistration] = useState('dynamic');
  const [confirmed, setConfirmed] = useState<{
    name: string;
    url: string;
    transport: Connection['transport'];
  }>();
  const [createdCredential, setCreatedCredential] = useState<Credential>();
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const navigate = useNavigate();
  const client = useQueryClient();
  const preset = presets[presetIndex]!;

  async function create(data: FormData) {
    const install = status.install_id!;
    const request = <T,>(path: string, method?: 'GET' | 'POST', body?: unknown) =>
      runnerRequest<T>(install, path, method, body);
    const credentials = String(data.get('token') ?? '');
    if (auth === 'bearer') validateSecret(credentials, 'Bearer token', true);
    const clientId =
      auth === 'oauth' && registration === 'pre-registered'
        ? requiredText(data, 'client', 'OAuth client ID', 1024)
        : undefined;
    const method = clientId ? readText(data, 'client-auth') : '';
    let credential = createdCredential;
    if (auth !== 'none') {
      const type = auth === 'oauth' ? 'mcp-oauth' : 'mcp-bearer';
      const config = {
        url: confirmed!.url,
        ...(type === 'mcp-oauth' && {
          registration_mode: registration,
          ...(clientId && { client_id: clientId, token_endpoint_auth_method: method || 'none' }),
          ...(preset.name === 'Notion' && { extra_parameters: { prompt: 'consent' } }),
          ...(preset.name === 'Angler MCP' && {
            scopes: ['openid', 'email'],
            extra_parameters: { access_type: 'offline', prompt: 'consent' },
          }),
        }),
      };
      const input = {
        type,
        config,
        name: confirmed!.name,
        groups: 'all' as const,
        sharing: { projects: 'none' as const },
        label: readText(data, 'connection-label') || 'destructive',
      };
      const identity = await runnerVaultIdentity(org, install);
      if (!credential) {
        credential = await request<Credential>('/credentials', 'POST', input);
        await verifyCredentialReceipt(credential, input, org, install, identity);
        setCreatedCredential(credential);
      }
      await saveCredentialSecrets(
        request,
        org,
        { ...credential, ...input },
        auth === 'bearer' ? { token: credentials } : {},
        { installId: install, identityKey: identity },
      );
    }
    const connection = await request<Connection>('/mcp-servers', 'POST', {
      ...confirmed,
      ...(credential && { credential_id: credential.id }),
      sharing: { projects: 'none' },
    });
    if (credential) rememberConnection(credential.id, connection.id);
    await client.invalidateQueries({ queryKey: ['runner-proxy', install] });
    await navigate({
      to: '/runner/connections/$connection',
      params: { connection: connection.id },
      search: { tab: auth === 'oauth' ? 'connect' : 'tools' },
      replace: true,
    });
  }

  return (
    <form
      className="grid gap-4"
      key={presetIndex}
      onSubmit={async (event) => {
        event.preventDefault();
        setError('');
        const form = event.currentTarget;
        const data = new FormData(form);
        if (confirmed) {
          setBusy(true);
          try {
            await create(data);
          } catch (problem) {
            setError(runnerError(problem));
          } finally {
            setBusy(false);
            for (const key of ['token', 'client-secret']) {
              const input = form.elements.namedItem(key);
              if (input instanceof HTMLInputElement) input.value = '';
            }
          }
        } else {
          try {
            const name = requiredText(data, 'name', 'Connection name');
            const url = endpoint(readText(data, 'url'), 'MCP server URL', {
              query: auth === 'oauth',
            });
            setConfirmed({
              name,
              url,
              transport: readText(data, 'transport') === 'sse' ? 'sse' : 'streamable-http',
            });
          } catch (problem) {
            setError(runnerError(problem));
          }
        }
      }}
    >
      <ConnectionProgress step={confirmed ? 1 : 0} />
      {confirmed ? (
        <FormSection
          title={auth === 'oauth' ? 'Prepare OAuth discovery' : 'Confirm the connection endpoint'}
          description={
            auth === 'oauth'
              ? 'The runner will look up this server’s authorization endpoints for you to confirm.'
              : 'Check the destination before allowing the runner to connect.'
          }
        >
          <div className="rounded-md border border-border bg-bg p-3">
            <p className="font-medium">{confirmed.name}</p>
            <p className="mt-1 text-sm break-all text-fg-muted">{confirmed.url}</p>
          </div>
          {auth === 'bearer' && (
            <Field label="Bearer token">
              <Input type="password" name="token" autoComplete="new-password" />
            </Field>
          )}
          {auth === 'oauth' && (
            <>
              <Field label="Client registration">
                <select
                  className={selectClasses}
                  value={registration}
                  onChange={(event) => setRegistration(event.target.value)}
                  disabled={busy || Boolean(createdCredential)}
                >
                  <option value="dynamic">Dynamic registration</option>
                  <option value="pre-registered">Use an existing OAuth app</option>
                </select>
              </Field>
              {registration === 'pre-registered' ? (
                <>
                  <Field label="OAuth client ID">
                    <Input name="client" />
                  </Field>
                  <Field label="Client authentication">
                    <select name="client-auth" className={selectClasses}>
                      <option value="none">Public client</option>
                      <option value="client_secret_basic">Client secret in header</option>
                      <option value="client_secret_post">Client secret in body</option>
                    </select>
                  </Field>
                </>
              ) : (
                <p className="text-sm text-fg-muted">
                  The provider determines client authentication during registration.
                </p>
              )}
            </>
          )}
          {auth !== 'none' && (
            <Field label="Credential classification">
              <select
                name="connection-label"
                className={selectClasses}
                defaultValue="destructive"
                disabled={busy}
              >
                <option value="read">Read</option>
                <option value="write">Write</option>
                <option value="destructive">Destructive</option>
              </select>
            </Field>
          )}
          <div className="flex flex-wrap gap-2">
            <Button type="submit" disabled={busy}>
              {busy
                ? 'Preparing…'
                : auth === 'oauth'
                  ? 'Continue to discovery'
                  : 'Confirm and connect'}
              <ArrowRight aria-hidden />
            </Button>
            {!createdCredential && (
              <Button
                type="button"
                variant="secondary"
                disabled={busy}
                onClick={() => {
                  setConfirmed(undefined);
                  setError('');
                }}
              >
                Back
              </Button>
            )}
          </div>
        </FormSection>
      ) : (
        <>
          <FormSection title="Choose a system">
            <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
              {presets.map((option, index) => (
                <label
                  key={option.name}
                  htmlFor={`connection-preset-${index}`}
                  className={`flex cursor-pointer items-start gap-2.5 rounded-md border p-3 ${presetIndex === index ? 'border-accent bg-accent-subtle' : 'border-border hover:bg-surface-raised'}`}
                >
                  <input
                    id={`connection-preset-${index}`}
                    name="preset"
                    type="radio"
                    className="mt-1 accent-accent"
                    checked={presetIndex === index}
                    onChange={() => {
                      setPresetIndex(index);
                      setAuth(option.auth);
                      setError('');
                    }}
                  />
                  <span className="block text-sm font-medium">
                    <option.icon className="mb-2 size-4 text-accent" aria-hidden />
                    {option.name}
                    <span className="block text-xs font-normal text-fg-muted">{option.detail}</span>
                  </span>
                </label>
              ))}
            </div>
          </FormSection>
          <FormSection title="Connection details">
            <div className="grid gap-3 sm:grid-cols-2">
              <Field label="Connection name">
                <Input name="name" defaultValue={preset.name === 'Custom' ? '' : preset.name} />
              </Field>
              <Field label="MCP server URL">
                <Input
                  name="url"
                  inputMode="url"
                  defaultValue={preset.url}
                  placeholder="https://mcp.example.com/mcp"
                />
              </Field>
              <Field label="Sign-in method">
                <select
                  className={selectClasses}
                  value={auth}
                  onChange={(event) => setAuth(event.target.value as Auth)}
                >
                  <option value="oauth">OAuth</option>
                  <option value="bearer">Bearer token</option>
                  <option value="none">No authentication</option>
                </select>
              </Field>
              <Field label="Transport">
                <select name="transport" className={selectClasses}>
                  <option value="streamable-http">Streamable HTTP</option>
                  <option value="sse">Server-sent events</option>
                </select>
              </Field>
            </div>
            <Button type="submit" className="w-fit">
              Continue
              <ArrowRight aria-hidden />
            </Button>
          </FormSection>
        </>
      )}
      <Problem>{error}</Problem>
    </form>
  );
}

function ConnectionDetail({
  connection,
  credential,
  status,
  org,
  editable,
  projects,
  tab,
}: Props & { connection: Connection; credential?: Credential; tab?: string }) {
  const navigate = useNavigate();
  const mutation = useRunnerMutation(status);
  const [message, setMessage] = useState('');
  const [error, setError] = useState('');
  const [deleting, setDeleting] = useState(false);
  const [rotating, setRotating] = useState(false);
  const path = `/mcp-servers/${connection.id}`;
  const credentialPath = `/credentials/${credential?.id ?? '-'}`;
  const setupActive =
    !connection.pinned_snapshot_id ||
    (tab === 'share' && Boolean(credential && activeConnection(credential.id)));
  const shareLinked = credential && activeConnection(credential.id) === connection.id;
  const selectedTab = tab === 'tools' || tab === 'share' ? tab : 'connection';
  const next = (page: string) => {
    setMessage('');
    setError('');
    return navigate({
      to: '/runner/connections/$connection',
      params: { connection: connection.id },
      search: { tab: page },
      replace: true,
    });
  };
  async function perform(action: () => Promise<void>) {
    setError('');
    setMessage('');
    try {
      await action();
    } catch (problem) {
      setError(runnerError(problem));
    }
  }
  return (
    <div className="grid gap-4">
      {setupActive && editable && (
        <ConnectionProgress
          step={tab === 'share' ? 4 : tab === 'tools' ? 3 : credential?.config.issuer ? 2 : 1}
        />
      )}
      <div
        className={
          panel +
          ' grid grid-cols-[auto_minmax(0,1fr)] items-start gap-3 sm:flex sm:items-center sm:p-5'
        }
      >
        <span className="grid size-11 shrink-0 place-items-center rounded-lg border border-border bg-bg text-accent">
          <Globe className="size-5" aria-hidden />
        </span>
        <div className="min-w-0 flex-1">
          <h2 className="text-lg font-semibold">{connection.name}</h2>
          <p className="text-sm break-all text-fg-muted">{connection.url}</p>
        </div>
        <div className="col-start-2 justify-self-start">
          <ConnectionStatusPill credential={credential} id={connection.credential_id} />
        </div>
      </div>
      <Tabs value={selectedTab} onValueChange={(value) => void next(value)}>
        <TabsList aria-label="Connection details">
          <TabsTrigger value="connection">Connection</TabsTrigger>
          <TabsTrigger value="tools">Tools</TabsTrigger>
          <TabsTrigger
            value="share"
            disabled={!connection.pinned_snapshot_id && selectedTab !== 'share'}
          >
            Sharing
          </TabsTrigger>
        </TabsList>
        <TabsContent value="connection" className="grid gap-4">
          <FormSection title="Sign-in and endpoints">
            <p className="text-sm text-fg-muted">
              {connectionStatus(credential, connection.credential_id)} ·{' '}
              {connection.transport === 'sse' ? 'Server-sent events' : 'Streamable HTTP'}
            </p>
            {credential?.type === 'mcp-oauth' && credential.status !== 'revoked' && (
              <OAuthCredentialActions
                credential={credential}
                install={status.install_id!}
                org={org}
                disabled={!editable || mutation.isPending}
                notify={setMessage}
                errors={setError}
              />
            )}
            {editable && credential?.type === 'mcp-bearer' && credential.status !== 'revoked' && (
              <>
                <Button
                  className="w-fit"
                  variant="secondary"
                  onClick={() => setRotating(!rotating)}
                >
                  {rotating ? 'Cancel token entry' : 'Replace bearer token'}
                </Button>
                {rotating && (
                  <form
                    className="grid gap-2"
                    onSubmit={(event) => {
                      event.preventDefault();
                      const form = event.currentTarget;
                      const token = String(new FormData(form).get('token') ?? '');
                      form.reset();
                      void perform(async () => {
                        validateSecret(token, 'Bearer token', true);
                        const install = status.install_id!;
                        await saveCredentialSecrets(
                          (route, method, body) => runnerRequest(install, route, method, body),
                          org,
                          credential,
                          { token },
                          {
                            installId: install,
                            identityKey: await runnerVaultIdentity(org, install),
                          },
                        );
                        await mutation.mutateAsync({
                          path: credentialPath + '/test',
                          method: 'POST',
                          body: {},
                        });
                        setRotating(false);
                        setMessage('Bearer token saved.');
                      });
                    }}
                  >
                    <Field label="Bearer token">
                      <Input type="password" name="token" autoComplete="new-password" />
                    </Field>
                    <Button className="w-fit" type="submit">
                      Save token
                    </Button>
                  </form>
                )}
              </>
            )}
            {editable && (
              <div className="flex flex-wrap gap-2">
                <Button
                  variant="secondary"
                  disabled={
                    mutation.isPending ||
                    Boolean(connection.credential_id && credential?.status !== 'active')
                  }
                  onClick={() =>
                    void perform(async () => {
                      const response = (await mutation.mutateAsync({
                        path: credential ? credentialPath + '/test' : path + '/discover',
                        method: 'POST',
                        body: {},
                      })) as { ok?: boolean };
                      setMessage(
                        response.ok === false
                          ? 'Connection test failed.'
                          : 'Connection test passed.',
                      );
                    })
                  }
                >
                  <ShieldCheck aria-hidden /> Test connection
                </Button>
                {(!connection.credential_id || credential?.status === 'active') && (
                  <Button variant="secondary" onClick={() => void next('tools')}>
                    Review tools
                    <ArrowRight aria-hidden />
                  </Button>
                )}
              </div>
            )}
            <Problem>{error}</Problem>
            {message && (
              <p role="status" className="text-sm">
                {message}
              </p>
            )}
          </FormSection>
          {editable && (
            <FormSection
              title="Remove connection"
              description="Projects and agents will lose access to this connection’s tools."
            >
              <div className="flex flex-wrap gap-2">
                <Button
                  variant={deleting ? 'destructive' : 'secondary'}
                  disabled={mutation.isPending}
                  onClick={() => {
                    if (!deleting) {
                      setDeleting(true);
                      return;
                    }
                    void perform(async () => {
                      await mutation.mutateAsync({ path, method: 'DELETE' });
                      await navigate({ to: '/runner/connections' });
                    });
                  }}
                >
                  {deleting ? 'Confirm delete' : 'Delete connection'}
                </Button>
                {deleting && (
                  <Button variant="secondary" onClick={() => setDeleting(false)}>
                    Cancel
                  </Button>
                )}
              </div>
            </FormSection>
          )}
        </TabsContent>
        <TabsContent value="tools">
          <ConnectionTools
            connection={connection}
            status={status}
            editable={editable && (!connection.credential_id || credential?.status === 'active')}
            onPinned={!connection.pinned_snapshot_id ? () => void next('share') : undefined}
          />
        </TabsContent>
        <TabsContent value="share">
          <FormSection
            title="Share with projects"
            description="Project admins can choose from the allowed tools on shared connections."
          >
            {editable ? (
              <form
                className="grid gap-3"
                onSubmit={(event) => {
                  event.preventDefault();
                  const fields = new FormData(event.currentTarget);
                  const sharing = readShare(fields, 'sharing', connection.sharing, projects);
                  void perform(async () => {
                    await mutation.mutateAsync({ path, method: 'PATCH', body: { sharing } });
                    if (credential && fields.get('share-credential') === 'on') {
                      await mutation.mutateAsync({
                        path: credentialPath,
                        method: 'PATCH',
                        body: { sharing },
                      });
                    }
                    if (credential) rememberConnection(credential.id, null);
                    setMessage('Project access saved.');
                  });
                }}
              >
                <ShareFields projects={projects} initial={connection.sharing} />
                {credential && (
                  <label className="flex items-start gap-2 text-sm">
                    <input
                      type="checkbox"
                      className="mt-1 accent-accent"
                      name="share-credential"
                      defaultChecked={Boolean(shareLinked)}
                    />
                    <span>
                      Share the linked credential with the same projects
                      <span className="mt-0.5 block text-xs text-fg-muted">
                        Agents need both the connection and its credential to call authenticated
                        tools.
                      </span>
                    </span>
                  </label>
                )}
                <Button className="w-fit" type="submit" disabled={mutation.isPending}>
                  Save project access
                </Button>
              </form>
            ) : (
              <p className="text-sm text-fg-muted">{shareLabel(connection, projects)}</p>
            )}
            <Problem>{error}</Problem>
            {message && (
              <p role="status" className="text-sm">
                {message}
              </p>
            )}
          </FormSection>
        </TabsContent>
      </Tabs>
      <Link to="/runner/connections" className={textLink + ' justify-self-start text-sm'}>
        All connections
      </Link>
    </div>
  );
}
