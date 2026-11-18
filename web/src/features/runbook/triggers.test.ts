import { describe, expect, it } from 'vitest';
import { triggersOf, webhookTriggerUrl } from './triggers';

const SOURCE = `
apiVersion: therunbook.app/v1
kind: Runbook
metadata: { id: deploy-check, title: Deploy check }
spec:
  triggers:
    schedule:
      - id: nightly
        cron: "0 2 * * *"
        timezone: America/Los_Angeles
        catch_up: 1h
        inputs: { service: api }
      - id: hourly_utc
        cron: "15 * * * *"
    webhook:
      - id: on_deploy
        payload:
          status: { type: string, required: true }
        when: "\${{ payload.status == 'success' }}"
        inputs: { service: "\${{ payload.repository.name }}" }
        dedupe: "\${{ payload.deployment.id }}"
  steps:
    - { id: check, title: Check }
`;

describe('triggersOf', () => {
  it('reads schedules and webhooks from a runbook source', () => {
    expect(triggersOf(SOURCE)).toEqual({
      schedule: [
        {
          id: 'nightly',
          cron: '0 2 * * *',
          timezone: 'America/Los_Angeles',
          catchUp: '1h',
          inputNames: ['service'],
        },
        {
          id: 'hourly_utc',
          cron: '15 * * * *',
          timezone: 'UTC',
          inputNames: [],
        },
      ],
      webhook: [
        {
          id: 'on_deploy',
          hasPayloadSchema: true,
          conditional: true,
          deduped: true,
          inputNames: ['service'],
        },
      ],
    });
  });

  it.each([
    undefined,
    '',
    'metadata: [',
    'spec: { triggers: [] }',
    'spec: { triggers: { schedule: [{ cron: "0 1 * * *" }] } }',
  ])('returns no triggers for %s', (source) => {
    expect(triggersOf(source)).toEqual({ schedule: [], webhook: [] });
  });
});

describe('webhookTriggerUrl', () => {
  it('builds the public POST URL and encodes each segment', () => {
    expect(
      webhookTriggerUrl({
        origin: 'https://runs.example.test/',
        project: 'prod ops',
        runbook: 'deploy-check',
        trigger: 'on_deploy',
      }),
    ).toBe(
      'https://runs.example.test/v1/projects/prod%20ops/runbooks/deploy-check/triggers/on_deploy',
    );
  });
});
