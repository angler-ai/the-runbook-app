// R0-6: a runner agent's step on the run page: which agent works it and
// with which model, what it cost, why it waits, the credentials released
// to it (never values), and a warning when credentials were released to a
// claim that never reported. Renders nothing for other steps.
import type { Schemas } from '../../api/client';

type StepView = Schemas['StepView'];
type StepCost = Schemas['StepCost'];

/** Why a runner step waits, in words. */
export function waitingText(reason: string, detail?: string | null): string {
  if (detail) return `Waiting: ${detail}`;
  if (reason === 'agent_paused') return 'Waiting: the agent is paused';
  if (reason === 'agent_outside_ceiling') return 'Waiting: the agent no longer allows this step';
  if (reason === 'runner_offline') return 'Waiting: the runner is offline';
  if (reason === 'limit:environment')
    return 'Waiting: an environment budget or concurrency cap is reached';
  if (reason.startsWith('limit:')) return `Waiting: the ${reason.slice(6)} limit is reached`;
  return `Waiting: ${reason}`;
}

/** `$0.32`, or `<$0.01` for a small non-zero amount. */
export function dollars(amount: number): string {
  if (amount > 0 && amount < 0.01) return '<$0.01';
  return `$${amount.toFixed(2)}`;
}

export function costText(cost: StepCost): string {
  const total = (cost.model_usd ?? 0) + (cost.compute_usd ?? 0);
  const tokens = (cost.input_tokens ?? 0) + (cost.output_tokens ?? 0);
  return tokens > 0
    ? `${dollars(total)} · ${tokens.toLocaleString('en-US')} tokens`
    : dollars(total);
}

export function RunnerStep({
  step,
  project,
  traceProject,
}: {
  step: StepView | undefined;
  project?: string;
  traceProject?: string | null;
}) {
  const runner = step?.runner;
  const cost = step?.cost;
  const traceId = runner?.trace_id;
  const validTrace = traceId && /^[0-9a-f]{32}$/.test(traceId) ? traceId : undefined;
  const traceUrl =
    validTrace && traceProject && /^[a-z][a-z0-9-]{4,29}$/.test(traceProject)
      ? `https://console.cloud.google.com/traces/list?tid=${validTrace}&project=${traceProject}`
      : undefined;
  if (!runner && !cost) return null;
  const waiting =
    runner?.waiting_reason && step?.status === 'ready'
      ? waitingText(runner.waiting_reason, runner.waiting_detail)
      : null;
  return (
    <div
      className="flex flex-col gap-1 text-xs text-fg-muted"
      data-testid={`runner-${step?.id ?? ''}`}
    >
      {runner ? (
        <div className="flex flex-wrap items-center gap-3">
          <span>
            Agent{' '}
            {project ? (
              <a
                className="font-mono text-accent underline"
                href={`/p/${encodeURIComponent(project)}/agents/${encodeURIComponent(runner.agent_id)}`}
              >
                {runner.agent_name}
              </a>
            ) : (
              <span className="font-mono">{runner.agent_name}</span>
            )}
            {runner.model ? (
              <>
                {' '}
                · model <span className="font-mono">{runner.model}</span>
              </>
            ) : null}
            {runner.agent_version_id ? (
              <>
                {' '}
                · version <span className="font-mono">{runner.agent_version_id}</span>
              </>
            ) : null}
          </span>
          {project && step && (
            <a
              className="text-accent underline"
              href={`/p/${encodeURIComponent(project)}/agents/transcripts/${encodeURIComponent(step.id)}`}
            >
              Transcript
            </a>
          )}
          {validTrace && (
            <span>
              Trace{' '}
              {traceUrl ? (
                <a className="font-mono text-accent underline" href={traceUrl}>
                  {validTrace}
                </a>
              ) : (
                <span className="font-mono">{validTrace}</span>
              )}
            </span>
          )}
          {cost ? <span>Cost {costText(cost)}</span> : null}
        </div>
      ) : cost ? (
        <span>Cost {costText(cost)}</span>
      ) : null}
      {waiting ? (
        <span role="status" className="text-warn">
          {waiting}
        </span>
      ) : null}
      {runner?.released_never_reported ? (
        <span role="alert" className="text-danger">
          Credentials were released to this step, but its claim ended without a report.
        </span>
      ) : null}
      {runner?.releases?.length ? (
        <ul aria-label="Credential releases" className="flex flex-col gap-0.5">
          {runner.releases.map((release) => (
            <li key={`${release.credential_id}-${release.at}`}>
              Released <span className="font-mono">{release.credential_id}</span> (version{' '}
              {release.credential_version}, {release.delivery}) to{' '}
              <span className="font-mono">{release.runner_id}</span>
              {release.revoked ? ` · revoked: ${release.revoked}` : null}
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}
