import { useState } from 'react';
import { Button } from '../../ui/primitives/button';
import { Input, fieldClasses } from '../../ui/primitives/input';
import { runnerError, type Row, type RunnerStatus } from '../runner/api';
import type { Credential } from '../runner/Credentials';
import { Field, list } from '../runner/presentation';
import type { Connection, EgressRule, Environment, Limits, Named } from '../runner/resources';
import {
  BudgetFields,
  EgressFields,
  FormSection,
  Problem,
  numberField,
  readBudgets,
  readEgress,
  readText,
  selectClasses,
} from '../runner/runnerForms';
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
    throw new Error(
      'Agent name must start with a lowercase letter and use at most 128 lowercase letters, digits, hyphens or underscores. “external” is reserved.',
    );
}

export function AgentConfiguration({
  agent,
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
  const [feedback, setFeedback] = useState<{ error: boolean; text: string }>();
  const [profile, setProfile] = useState(config?.profiles?.[0] ?? '');
  const [environmentId, setEnvironmentId] = useState(config?.environment_id ?? '');
  const [selectedTools, setTools] = useState(config?.mcp ?? []);
  const override = config?.environment_overrides;
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
      throw new Error(
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
    if (!group_id) throw new Error('Runner group must be selected.');
    const sandboxSize = readText(data, 'sandbox');
    const size = sizes.find((value) => value === sandboxSize);
    if (environment && size && sizes.indexOf(size) > sizes.indexOf(environment.size))
      throw new Error(`Sandbox size cannot exceed ${environment.size} in ${environment.name}.`);
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
      throw new Error(`Egress hosts and access must be allowed by ${environment.name}.`);
    const limits = readBudgets(data, '', caps);
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
      throw new Error(
        `Run cap (USD) cannot exceed ${environment.budgets.max_run_cost_usd} in ${environment.name}.`,
      );
    const approved = data.getAll('required-credential').map(String);
    if (requiredBindings.length !== grantOptions.length)
      throw new Error(
        'The selected environment or connection requires a credential that is not shared with this project.',
      );
    if (requiredBindings.some((binding) => !approved.includes(bindingKey(binding))))
      throw new Error(
        'Select each required credential to use the chosen environment and connection tools.',
      );
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
  return (
    <form
      className="grid gap-4"
      onSubmit={(event) => {
        event.preventDefault();
        if (!canEdit || pending) return;
        const data = new FormData(event.currentTarget);
        void perform(() => save(readConfig(data)), 'Configuration saved.');
      }}
    >
      <fieldset className="grid min-w-0 gap-4" disabled={pending || !canEdit}>
        <FormSection title="Agent details">
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
            <FormSection title="Model and sandbox">
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
              title="Agent budgets"
              description="Environment and organization caps also apply."
            >
              <BudgetFields
                initial={{
                  ...config?.limits,
                  max_run_cost_usd: config?.environment_version_id
                    ? override?.max_run_cost_usd
                    : config?.limits?.max_run_cost_usd,
                }}
                keys={caps}
              />
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
            {status && projectId && (
              <FormSection
                title="Connections and tools"
                description="Enable a connection to choose the specific tools this agent can call."
              >
                <AgentToolPicker
                  projectId={projectId}
                  selected={selectedTools}
                  onChange={setTools}
                  status={status}
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
            <FormSection title="Egress allow-list">
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
        {canEdit && (
          <div className="flex flex-wrap gap-2">
            <Button type="submit">Save configuration</Button>
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
      </fieldset>
      <Problem>{feedback?.error && feedback.text}</Problem>
      {feedback && !feedback.error && (
        <p role="status" className="text-sm text-success">
          {feedback.text}
        </p>
      )}
    </form>
  );
}
