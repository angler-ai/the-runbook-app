import { useState } from 'react';
import { Button } from '../../ui/primitives/button';
import { Input, fieldClasses } from '../../ui/primitives/input';
import {
  runnerError,
  useRunnerQuery,
  type Items,
  type Row,
  type RunnerStatus,
} from '../runner/api';
import type { Credential } from '../runner/Credentials';
import { Field, list } from '../runner/presentation';
import type {
  AgentToolCall,
  Connection,
  EgressRule,
  Environment,
  Limits,
  Named,
} from '../runner/resources';
import {
  BudgetFields,
  EgressFields,
  FieldError,
  FormSection,
  RunnerForm,
  ReviewBudgetChanges,
  increasedCaps,
  type CapChange,
  textLink,
  Problem,
  numberField,
  readBudgets,
  readEgress,
  readText,
  selectClasses,
} from '../runner/runnerForms';
import type { ActivityData } from './AgentActivity';
import { AgentToolPicker, type ToolBinding } from './AgentToolPicker';

type AgentLimits = Limits & {
  max_cost_usd?: number;
  max_tokens?: number;
  timeout_s?: number;
  [key: string]: unknown;
};
type AgentOverrides = {
  size?: Environment['size'];
  timeout_s?: number;
  egress?: EgressRule[];
  max_cost_usd?: number;
  max_run_cost_usd?: number;
};

export type Agent = {
  id: string;
  name: string;
  description: string;
  status: string;
  group_id: string;
  updated_at?: string;
  config?:
    | (Row & {
        instructions?: string;
        skills?: string[];
        profiles?: string[];
        mcp?: ToolBinding[];
        credentials?: { credential_id: string; delivery: string }[];
        environment_id?: string | null;
        environment_version_id?: string;
        environment_overrides?: AgentOverrides;
        egress?: EgressRule[];
        limits?: AgentLimits;
        sandbox?: { size: Environment['size'] };
      })
    | null;
  ceiling?: {
    tools?: { server_id: string; name: string; label: string }[];
    credentials?: { id: string; name: string }[];
  } | null;
};

const caps: (keyof Limits)[] = [
  'cost_per_day_usd',
  'cost_per_month_usd',
  'concurrency',
  'max_run_cost_usd',
  'alert_threshold_percent',
];
const sizes = ['small', 'medium', 'large'] as const;
export function validateAgentName(name: string) {
  if (name === 'external' || !/^[a-z][a-z0-9_-]{0,127}$/.test(name))
    throw new FieldError(
      'name',
      'Agent name must start with a lowercase letter and use at most 128 lowercase letters, digits, hyphens or underscores. “external” is reserved.',
    );
}

function GuidedToolResult({
  status,
  projectId,
  agentId,
  connection,
  tools,
  startedAt,
}: {
  status: RunnerStatus;
  projectId: string;
  agentId: string;
  connection: Connection;
  tools: string[];
  startedAt: number;
}) {
  const calls = useRunnerQuery<Items<AgentToolCall>>(
    status,
    `/projects/${projectId}/agents/${agentId}/tool-calls`,
  );
  const result = calls.data?.items.find(
    (call) =>
      call.server_id === connection.id &&
      tools.includes(call.tool) &&
      Date.parse(call.created_at) >= startedAt,
  );
  const activity = useRunnerQuery<ActivityData>(
    status,
    `/projects/${projectId}/agents/${agentId}/activity`,
    Boolean(result),
  );
  const executionId = activity.data?.recent?.find(
    ({ step_run_id }) => step_run_id === result?.step_run_id,
  )?.execution_id;
  return (
    <div className="grid gap-2 text-sm">
      <p role="status">
        {result
          ? result.error
            ? `${connection.name} ${result.tool} failed.`
            : `${connection.name} ${result.tool} succeeded.`
          : calls.error
            ? 'Could not load tool call results.'
            : 'Waiting for a tool call.'}
      </p>
      {executionId && result && (
        <a
          className={textLink}
          href={`/x/${encodeURIComponent(executionId)}?view=steps&step=${encodeURIComponent(result.step_run_id)}`}
        >
          View test run
        </a>
      )}
      <Button
        type="button"
        variant="secondary"

        size="sm"
        className="w-fit"
        onClick={() => void calls.refetch()}
        disabled={calls.isFetching}
      >
        Check latest result
      </Button>
    </div>
  );
}

export function AgentConfiguration({
  agent,
  guided,
  projectSlug,
  canEdit,
  admin,
  groups,
  profiles,
  environments = [],
  connections = [],
  credentials = [],
  projectId,
  status,
  save,
  operate,
}: {
  agent: Agent;
  guided?: Connection;
  projectSlug?: string;
  canEdit: boolean;
  admin: boolean;
  groups: Named[];
  profiles: Named[];
  environments?: Environment[];
  connections?: Connection[];
  credentials?: Credential[];
  projectId?: string;
  status?: RunnerStatus;
  save: (body: Row) => Promise<void>;
  operate: (action: 'pause' | 'archive') => Promise<void>;
}) {
  const config = agent.config;
  const [pending, setPending] = useState(false);
  const [confirmArchive, setConfirmArchive] = useState(false);
  const [review, setReview] = useState<{ body: Row; tools: string[]; changes: CapChange[] }>();
  const [completed, setCompleted] = useState<{ tools: string[]; startedAt: number }>();
  const [feedback, setFeedback] = useState<{ error: boolean; text: string }>();
  const [profile, setProfile] = useState(config?.profiles?.[0] ?? '');
  const [environmentId, setEnvironmentId] = useState(config?.environment_id ?? '');
  const [selectedTools, setTools] = useState(config?.mcp ?? []);
  const override = config?.environment_overrides;
  const guidedCredential = credentials.find(
    (item) => guided?.credential_id === item.id && item.status === 'active',
  );
  const hasGuidedCredential =
    !guided?.credential_id ||
    (guidedCredential &&
      config?.credentials?.some(
        ({ credential_id, delivery }) =>
          credential_id === guidedCredential.id && delivery === 'mcp',
      ));
  const savedTools =
    guided?.pinned_snapshot_id && hasGuidedCredential
      ? (config?.mcp
          ?.filter(
            ({ server_id, tool }) =>
              server_id === guided.id && !guided.never_allowed?.includes(tool),
          )
          .map(({ tool }) => tool) ?? [])
      : [];
  const verification =
    completed ??
    (savedTools.length > 0
      ? { tools: savedTools, startedAt: Date.parse(agent.updated_at ?? '') || 0 }
      : undefined);
  const previousCaps = {
    ...config?.limits,
    max_run_cost_usd: config?.environment_version_id
      ? override?.max_run_cost_usd
      : config?.limits?.max_run_cost_usd,
    max_step_cost_usd:
      override?.max_cost_usd ??
      (config?.environment_version_id ? undefined : config?.limits?.max_cost_usd),
  };
  const inheritedEgress = config?.environment_version_id ? override?.egress : config?.egress;
  const [setHosts, setSetHosts] = useState(inheritedEgress != null);
  const environment = environments.find((item) =>
    environmentId ? item.id === environmentId : item.is_default,
  );
  const toolConnections = connections.filter((connection) =>
    selectedTools.some((tool) => tool.server_id === connection.id),
  );
  const requiredBindings = [
    ...[...new Set(Object.values(environment?.credential_env ?? {}))].map((credential_id) => ({
      credential_id,
      delivery: 'proxy',
    })),
    ...[
      ...new Set(
        toolConnections.flatMap((connection) =>
          connection.credential_id ? [connection.credential_id] : [],
        ),
      ),
    ].map((credential_id) => ({ credential_id, delivery: 'mcp' })),
  ].filter(
    (binding) =>
      !config?.credentials?.some(
        (item) =>
          item.credential_id === binding.credential_id && item.delivery === binding.delivery,
      ),
  );
  const bindingKey = (binding: { credential_id: string; delivery: string }) =>
    `${binding.delivery}:${binding.credential_id}`;
  const grantOptions = requiredBindings.flatMap((binding) => {
    const credential = credentials.find(
      (item) => item.id === binding.credential_id && item.delivery.includes(binding.delivery),
    );
    return credential ? [{ ...binding, name: credential.name, label: credential.label }] : [];
  });

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
  function readConfig(data: FormData) {
    const skills = list(readText(data, 'skills'));
    if (
      skills.length > 16 ||
      skills.some((skill) => !/^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/.test(skill))
    )
      throw new FieldError(
        'skills',
        'Skills must contain at most 16 project skill names, using lowercase letters, digits, and hyphens.',
      );
    const body: Row = {
      description: data.get('description'),
      instructions: data.get('instructions'),
      skills,
    };
    if (!admin) return body;
    const name = readText(data, 'name');
    validateAgentName(name);
    const group_id = readText(data, 'group_id');
    if (!group_id) throw new FieldError('group_id', 'Runner group must be selected.');
    const sandboxSize = readText(data, 'sandbox');
    const size = sizes.find((value) => value === sandboxSize);
    if (environment && size && sizes.indexOf(size) > sizes.indexOf(environment.size))
      throw new FieldError(
        'sandbox',
        `Sandbox size cannot exceed ${environment.size} in ${environment.name}.`,
      );
    const egress = setHosts ? readEgress(data) : environment ? null : [];
    if (
      setHosts &&
      environment?.egress &&
      egress?.some(
        (rule) =>
          !environment.egress?.some(
            (allowed) =>
              allowed.host === rule.host && (allowed.label === 'write' || rule.label === 'read'),
          ),
      )
    )
      throw new FieldError(
        'egress:host',
        `Egress hosts and access must be allowed by ${environment.name}.`,
      );
    const limits = readBudgets(data, '', caps, agent.name);
    const timeout = numberField(data, 'timeout', 'Step timeout', {
      min: 1,
      max: environment?.timeout_s ?? 86400,
      integer: true,
    });
    const maxCost = numberField(data, 'step', 'Step cap (USD)', {
      max: environment?.budgets.max_step_cost_usd ?? 99_999_999.99,
    });
    if (
      limits.max_run_cost_usd != null &&
      environment?.budgets.max_run_cost_usd != null &&
      limits.max_run_cost_usd > environment.budgets.max_run_cost_usd
    )
      throw new FieldError(
        'max_run_cost_usd',
        `Run cap (USD) cannot exceed ${environment.budgets.max_run_cost_usd} in ${environment.name}.`,
      );
    const approved = data.getAll('required-credential').map(String);
    if (requiredBindings.length !== grantOptions.length)
      throw new FieldError(
        toolConnections.length ? 'connection-tools' : 'environment',
        'The selected environment or connection requires a credential that is not shared with this project.',
      );
    const missing = grantOptions.flatMap((binding, index) =>
      approved.includes(bindingKey(binding))
        ? []
        : [
            {
              name: 'required-credential',
              index,
              message: `Grant ${binding.name} to use the selected tools or environment.`,
            },
          ],
    );
    if (missing.length) throw new FieldError(missing);
    const existingLimits = { ...config?.limits };
    for (const key of caps) delete existingLimits[key];
    return {
      ...body,
      name,
      group_id,
      status: readText(data, 'status'),
      profiles: profile
        ? [
            profile,
            ...data
              .getAll('extra-profile')
              .map(String)
              .filter((id) => id !== profile),
          ]
        : [],
      environment_id: environmentId || null,
      limits: {
        ...existingLimits,
        ...Object.fromEntries(Object.entries(limits).filter(([, value]) => value != null)),
        timeout_s: timeout ?? (environment ? null : (config?.limits?.timeout_s ?? 600)),
        max_cost_usd: maxCost ?? (environment ? null : (config?.limits?.max_cost_usd ?? 1)),
        max_tokens: numberField(data, 'tokens', 'Maximum tokens', {
          min: 1,
          max: 2_147_483_647,
          integer: true,
          required: true,
        }),
      },
      sandbox: size ? { size } : environment ? null : { size: config?.sandbox?.size ?? 'small' },
      egress,
      mcp: selectedTools,
      ...(requiredBindings.length > 0 && {
        credentials: [...(config?.credentials ?? []), ...requiredBindings],
      }),
    };
  }
  const profileOptions = [
    ...profiles,
    ...(config?.profiles ?? [])
      .filter((id) => !profiles.some((item) => item.id === id))
      .map((id) => ({ id, name: 'Unavailable profile' })),
  ];
  async function saveConfiguration(next: Row, tools: string[]) {
    await perform(
      async () => {
        const startedAt = Date.now();
        await save(next);
        if (guided) setCompleted({ tools, startedAt });
      },
      guided && tools.length
        ? `Agent ${String(next.name ?? agent.name)} can now use ${guided.name} ${tools.join(', ')}.`
        : 'Configuration saved.',
    );
    setReview(undefined);
  }

  const toolSections = (
    <>
      {status && projectId && (
        <FormSection
          id="agent-tools"
          title="Connections and tools"
          description="Enable a connection to choose the specific tools this agent can call."
        >
          <AgentToolPicker
            projectId={projectId}
            selected={selectedTools}
            onChange={setTools}
            status={status}
            guidedConnection={guided?.id}
          />
        </FormSection>
      )}
      {requiredBindings.length > 0 && (
        <FormSection
          title="Required credentials"
          description="Grant the credentials required by this environment and these connection tools."
        >
          {grantOptions.map((item) => (
            <label key={bindingKey(item)} className="flex gap-2 text-sm">
              <input
                name="required-credential"
                type="checkbox"
                className="accent-accent"
                value={bindingKey(item)}
              />
              <span>
                {item.name}{' '}
                <span className="text-xs text-fg-muted">
                  · {item.delivery === 'mcp' ? 'MCP' : 'Proxy'} · {item.label}
                </span>
              </span>
            </label>
          ))}
          {requiredBindings.length > grantOptions.length && (
            <p role="status" className="text-sm text-warn">
              Ask a runner administrator to share the required credentials with this project.
            </p>
          )}
        </FormSection>
      )}
    </>
  );
  return (
    <RunnerForm
      className="grid gap-4"
      onSubmit={async (event) => {
        event.preventDefault();
        if (!canEdit || pending) return;
        const data = new FormData(event.currentTarget);
        setFeedback(undefined);
        const next = readConfig(data);
        const tools = guided
          ? selectedTools.filter(({ server_id }) => server_id === guided.id).map(({ tool }) => tool)
          : [];
        if (guided && !tools.length)
          throw new FieldError('connection-tools', `Choose a tool from ${guided.name}.`);
        const limits = next.limits as AgentLimits | undefined;
        const changes = limits
          ? increasedCaps(agent.name, previousCaps, {
              ...limits,
              max_step_cost_usd: limits.max_cost_usd,
            })
          : [];
        if (changes.length) setReview({ body: next, tools, changes });
        else await saveConfiguration(next, tools);
      }}
    >
      {review && (
        <ReviewBudgetChanges
          changes={review.changes}
          pending={pending}
          close={() => setReview(undefined)}
          onConfirm={() => saveConfiguration(review.body, review.tools)}
        />
      )}
      {guided && (
        <FormSection title={`Set up ${guided.name} for ${agent.name}`}>
          {verification ? (
            <p role="status" className="text-sm font-medium text-success">
              Agent {agent.name} can now use {guided.name} {verification.tools.join(', ')}.
            </p>
          ) : (
            <p className="text-sm text-fg-muted">
              Choose tools, grant the required credentials and save the configuration.
            </p>
          )}
          {verification && projectSlug && (
            <>
              {agent.status !== 'active' && (
                <p className="text-sm text-warn">
                  Set the agent to Active and choose a model profile before running a test.
                </p>
              )}
              <p className="text-sm">
                Run a read-only search task with this agent, then check the result here.
              </p>
              <div className="flex flex-wrap gap-3 text-sm">
                <a className={textLink} href={`/p/${projectSlug}/runbooks`}>
                  Open project runbooks
                </a>
                <a
                  className={textLink}
                  href={`/runner/overview?project=${encodeURIComponent(projectId!)}&agent=${encodeURIComponent(agent.id)}&connection=${encodeURIComponent(guided.id)}`}
                >
                  View setup checklist
                </a>
              </div>
              {status && projectId && (
                <GuidedToolResult
                  status={status}
                  projectId={projectId}
                  agentId={agent.id}
                  connection={guided}
                  tools={verification.tools}
                  startedAt={verification.startedAt}
                />
              )}
            </>
          )}
        </FormSection>
      )}
      {admin && (
        <nav aria-label="Agent configuration sections" className="flex flex-wrap gap-3 text-sm">
          <a className={textLink} href="#agent-details">
            Details
          </a>
          <a className={textLink} href="#agent-model">
            Model and sandbox
          </a>
          <a className={textLink} href="#agent-budgets">
            Budgets
          </a>
          {status && projectId && (
            <a className={textLink} href="#agent-tools">
              Tools and credentials
            </a>
          )}
          <a className={textLink} href="#agent-egress">
            Egress
          </a>
        </nav>
      )}
      <fieldset className="grid min-w-0 gap-4" disabled={pending || !canEdit}>
        {admin && guided && toolSections}
        <FormSection id="agent-details" title="Agent details">
          {admin && (
            <div className="grid gap-3 sm:grid-cols-2">
              <Field label="Agent name">
                <Input name="name" defaultValue={agent.name} />
              </Field>
              <Field label="Status">
                <select className={selectClasses} name="status" defaultValue={agent.status}>
                  {['draft', 'active', 'paused', 'archived'].map((state) => (
                    <option key={state} value={state}>
                      {state[0]!.toUpperCase() + state.slice(1)}
                    </option>
                  ))}
                </select>
              </Field>
            </div>
          )}
          <Field label="Description">
            <Input name="description" defaultValue={agent.description} />
          </Field>
          <Field label="Instructions">
            <textarea
              name="instructions"
              className={fieldClasses + ' min-h-28 py-2'}
              defaultValue={config?.instructions}
            />
          </Field>
          <Field label="Skills (comma-separated)">
            <Input name="skills" defaultValue={config?.skills?.join(', ')} />
          </Field>
        </FormSection>
        {admin && (
          <>
            <FormSection id="agent-model" title="Model and sandbox">
              <div className="grid gap-3 sm:grid-cols-2">
                <Field label="Default model profile">
                  <select
                    className={selectClasses}
                    value={profile}
                    onChange={(event) => setProfile(event.target.value)}
                  >
                    <option value="">No default profile</option>
                    {profileOptions.map((item) => (
                      <option key={item.id} value={item.id}>
                        {item.name}
                      </option>
                    ))}
                  </select>
                </Field>
                <Field label="Runner group">
                  <select className={selectClasses} name="group_id" defaultValue={agent.group_id}>
                    {groups.map((item) => (
                      <option key={item.id} value={item.id}>
                        {item.name}
                      </option>
                    ))}
                  </select>
                </Field>
                <Field label="Environment">
                  <select
                    className={selectClasses}
                    name="environment"
                    value={environmentId}
                    onChange={(event) => setEnvironmentId(event.target.value)}
                  >
                    <option value="">Organization default</option>
                    {environmentId && !environments.some((item) => item.id === environmentId) && (
                      <option value={environmentId}>Current environment</option>
                    )}
                    {environments.map((item) => (
                      <option key={item.id} value={item.id}>
                        {item.name}
                      </option>
                    ))}
                  </select>
                </Field>
                <Field label="Sandbox size">
                  <select
                    className={selectClasses}
                    name="sandbox"
                    defaultValue={
                      override?.size ??
                      (config?.environment_version_id ? '' : config?.sandbox?.size) ??
                      ''
                    }
                  >
                    <option value="">Environment default</option>
                    {sizes.map((size) => (
                      <option key={size}>{size}</option>
                    ))}
                  </select>
                </Field>
              </div>
              {profile && profileOptions.some((item) => item.id !== profile) && (
                <fieldset className="grid gap-2">
                  <legend className="mb-2 text-sm font-medium">
                    Additional allowed model profiles
                  </legend>
                  {profileOptions
                    .filter((item) => item.id !== profile)
                    .map((item) => (
                      <label className="flex gap-2 text-sm" key={item.id}>
                        <input
                          name="extra-profile"
                          type="checkbox"
                          className="accent-accent"
                          value={item.id}
                          defaultChecked={config?.profiles?.includes(item.id)}
                        />{' '}
                        {item.name}
                      </label>
                    ))}
                </fieldset>
              )}
            </FormSection>
            <FormSection
              id="agent-budgets"
              title="Agent budgets"
              description="Environment and organization caps also apply."
            >
              <BudgetFields initial={previousCaps} keys={caps} />
              <div className="grid gap-3 sm:grid-cols-3">
                <Field label="Step cap (USD)">
                  <Input
                    name="step"
                    inputMode="decimal"
                    defaultValue={
                      override?.max_cost_usd ??
                      (config?.environment_version_id ? '' : (config?.limits?.max_cost_usd ?? ''))
                    }
                    placeholder="Environment default"
                  />
                </Field>
                <Field label="Step timeout (seconds)">
                  <Input
                    name="timeout"
                    inputMode="numeric"
                    defaultValue={
                      override?.timeout_s ??
                      (config?.environment_version_id ? '' : (config?.limits?.timeout_s ?? ''))
                    }
                    placeholder="Environment default"
                  />
                </Field>
                <Field label="Maximum tokens">
                  <Input
                    name="tokens"
                    inputMode="numeric"
                    defaultValue={config?.limits?.max_tokens ?? 16384}
                  />
                </Field>
              </div>
            </FormSection>
            {!guided && toolSections}
            <FormSection id="agent-egress" title="Egress allow-list">
              <label className="flex gap-2 text-sm">
                <input
                  type="checkbox"
                  aria-describedby="agent-egress-hint"
                  checked={setHosts}
                  onChange={(event) => setSetHosts(event.target.checked)}
                  className="accent-accent"
                />{' '}
                Set an explicit host allow-list
              </label>
              <p id="agent-egress-hint" className="text-xs text-fg-muted">
                Sandbox HTTPS requires an allowed host and exactly one released proxy credential
                bound to it.
              </p>
              {setHosts && <EgressFields initial={inheritedEgress} />}
            </FormSection>
          </>
        )}
      </fieldset>
      {canEdit && (
        <div className="sticky bottom-0 z-10 flex flex-wrap gap-2 border-t border-border bg-bg py-2">
          <Button type="submit" disabled={pending}>
            Save configuration
          </Button>
        </div>
      )}
      {canEdit && admin && (
        <div className="flex flex-wrap gap-2">
          {admin && agent.status === 'active' && (
            <Button
              type="button"
              variant="secondary"
              onClick={() => void perform(() => operate('pause'), 'Agent paused.')}
            >
              Pause agent
            </Button>
          )}
          {admin && agent.status !== 'archived' && (
            <Button
              type="button"
              variant={confirmArchive ? 'destructive' : 'secondary'}
              onClick={() =>
                confirmArchive
                  ? void perform(() => operate('archive'), 'Agent archived.')
                  : setConfirmArchive(true)
              }
            >
              {confirmArchive ? 'Confirm archive' : 'Archive agent'}
            </Button>
          )}
          {confirmArchive && (
            <Button type="button" variant="secondary" onClick={() => setConfirmArchive(false)}>
              Cancel
            </Button>
          )}
        </div>
      )}
      <Problem>{feedback?.error && feedback.text}</Problem>
      {!guided && feedback && !feedback.error && (
        <p role="status" className="text-sm text-success">
          {feedback.text}
        </p>
      )}
    </RunnerForm>
  );
}
