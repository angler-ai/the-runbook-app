import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import type { Schemas } from '../../api/client';
import { RunnerStep, costText, waitingText } from './RunnerStep';

type StepView = Schemas['StepView'];

function step(overrides: Partial<StepView>): StepView {
  return {
    schema_revision: 1,
    id: 'sr_01JD0000000000000000000001',
    path: 'check',
    status: 'ready',
    verdict: null,
    outputs: {},
    summary: null,
    actor: null,
    claim: null,
    attempt: 1,
    satisfied: false,
    exhausted: false,
    checklist: [],
    amended: false,
    amendments: [],
    stale: false,
    eligible_recipients: [],
    ...overrides,
  } as StepView;
}

describe('a runner step on the run page', () => {
  it('shows_waiting_reason_text', () => {
    render(
      <RunnerStep
        step={step({
          runner: {
            agent_id: 'agt_1',
            agent_name: 'deploy-checker',
            model: 'deep',
            waiting_reason: 'limit:project',
            waiting_detail: 'project ops is at its 5 concurrent steps',
          },
        })}
      />,
    );
    expect(screen.getByRole('status')).toHaveTextContent(
      'Waiting: project ops is at its 5 concurrent steps',
    );
    expect(screen.getByText(/deploy-checker/)).toBeInTheDocument();
    expect(waitingText('agent_paused')).toBe('Waiting: the agent is paused');
    expect(waitingText('limit:group')).toBe('Waiting: the group limit is reached');
  });

  it('displays the environment budget name and cap while a step waits', () => {
    render(
      <RunnerStep
        step={step({
          runner: {
            agent_id: 'agent',
            agent_name: 'quality-check',
            waiting_reason: 'limit:environment',
            waiting_detail: 'Environment Warehouse analytics: daily budget $20.00 reached',
          },
        })}
      />,
    );
    expect(screen.getByRole('status')).toHaveTextContent(
      'Waiting: Environment Warehouse analytics: daily budget $20.00 reached',
    );
  });

  it('hides the waiting reason once the step is claimed', () => {
    render(
      <RunnerStep
        step={step({
          status: 'claimed',
          runner: {
            agent_id: 'agt_1',
            agent_name: 'deploy-checker',
            waiting_reason: 'runner_offline',
          },
        })}
      />,
    );
    expect(screen.queryByRole('status')).toBeNull();
  });

  it('shows_released_never_reported_flag', () => {
    render(
      <RunnerStep
        step={step({
          status: 'ready',
          runner: {
            agent_id: 'agt_1',
            agent_name: 'deploy-checker',
            agent_version_id: 'v3',
            released_never_reported: true,
            releases: [
              {
                credential_id: 'cred_dd',
                credential_version: 2,
                delivery: 'proxy',
                runner_id: 'rn_1',
                at: 1,
                revoked: 'lease_ended',
              },
            ],
          },
        })}
      />,
    );
    expect(screen.getByRole('alert')).toHaveTextContent(
      'Credentials were released to this step, but its claim ended without a report.',
    );
    const releases = screen.getByRole('list', { name: 'Credential releases' });
    expect(releases).toHaveTextContent('cred_dd');
    expect(releases).toHaveTextContent('proxy');
    expect(releases).toHaveTextContent('revoked: lease_ended');
  });

  it('shows the step cost, and nothing for a step with neither', () => {
    const { container, rerender } = render(<RunnerStep step={step({})} />);
    expect(container).toBeEmptyDOMElement();
    rerender(
      <RunnerStep
        step={step({
          status: 'succeeded',
          cost: {
            model_usd: 0.25,
            compute_usd: 0.05,
            input_tokens: 1000,
            output_tokens: 200,
            cached_tokens: 0,
          },
        })}
      />,
    );
    expect(screen.getByText('Cost $0.30 · 1,200 tokens')).toBeInTheDocument();
    expect(
      costText({
        model_usd: 0.001,
        compute_usd: 0,
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
      }),
    ).toBe('<$0.01');
  });
});
