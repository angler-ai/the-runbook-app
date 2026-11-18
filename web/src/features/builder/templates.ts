// UI-4: the runbook templates the creation flow offers, in one place. Add a
// template by adding an entry: its card text, the kinds it shows off, and
// its YAML with `__ID__`, `__TITLE__` and (optionally) `__APPROVERS__`
// standing for the runbook's ID, title and the approvers block.
import type { PaletteKind } from './kinds';

export type TemplateId = 'blank' | 'post-deploy' | 'customer-onboarding';

/** What `/p/$project/runbooks/new` opens with (the creation flow's choice). */
export interface NewRunbookSearch {
  template?: string;
  name?: string;
  id?: string;
}

export interface TemplateParams {
  /** metadata.id (kebab-case). */
  id: string;
  title: string;
  /** The person creating it, as the first approver of a template's approvals. */
  me?: string;
}

export interface RunbookTemplate {
  id: TemplateId;
  title: string;
  description: string;
  /** Who does the work, in a few words. */
  who: string;
  /** Kinds the template shows off (icons on its card). */
  kinds: PaletteKind[];
  yaml: string;
}

const BLANK = `apiVersion: therunbook.app/v1
kind: Runbook
metadata:
  id: __ID__
  title: __TITLE__
spec:
  steps: []
`;

const POST_DEPLOY = `apiVersion: therunbook.app/v1
kind: Runbook
metadata:
  id: __ID__
  title: __TITLE__
  description: An agent checks health, error rates and latency after a production deploy.
  tags: [deploy, agentic]
spec:
  execution:
    title: "\${{ inputs.service }} \${{ inputs.version }}: post-deploy checks"
  inputs:
    service: { type: string, required: true }
    version: { type: string, required: true }
    regions: { type: list, items: { type: string }, default: [us-east, eu-west] }
  defaults: { actor: agent, timeout: 15m }
  steps:
    - id: health
      title: Health endpoint answers 200
      check: true
      instructions: |
        Call the health endpoint of \${{ inputs.service }} three times, 10 seconds apart.
        Pass when every call answers 200 within 500 ms; record the slowest latency.
      outputs:
        status_code: { type: integer, required: true }
    - id: error_rates
      title: Error rate per region
      foreach:
        items: "\${{ inputs.regions }}"
        as: region
        concurrency: 3
        max_items: 10
        steps:
          - id: error_rate
            title: "Error rate in \${{ region }}"
            check: true
            instructions: Pass below 0.5%, warn up to 1%, fail above 1%. Link the dashboard you read.
            evidence: { required: [link] }
    - id: latency
      title: p95 latency under 500 ms
      check: true
      instructions: Compare p95 latency in the 15 minutes since the deploy with the 15 minutes before.
    - id: gate
      title: Stop on a failed check
      switch:
        cases:
          - when: "\${{ steps.health.verdict == 'fail' || steps.error_rates.verdict == 'fail' || steps.latency.verdict == 'fail' }}"
            steps:
              - id: checks_failed
                end: { status: failed, message: A post-deploy check failed. }
  finally:
    - id: summary
      title: Summarize the deploy
      instructions: Post a short summary to the deploy channel with each check's verdict and the numbers behind it.
`;

const CUSTOMER_ONBOARDING = `apiVersion: therunbook.app/v1
kind: Runbook
metadata:
  id: __ID__
  title: __TITLE__
  description: Takes a signed customer from kickoff to go-live with your team, agents and the customer.
  tags: [customers, onboarding]
spec:
  execution:
    title: "Onboard \${{ inputs.customer }}"
  inputs:
    customer: { type: string, required: true }
    plan: { type: enum, options: [starter, growth, enterprise], required: true }
  steps:
    - id: kickoff
      title: Kickoff call with the customer
      actor: human
      instructions: Agree goals, name the customer's technical contact and propose a go-live date.
      checklist: [Goals agreed, Technical contact named, Go-live date proposed]
    - id: provision
      title: Provision the workspace
      actor: agent
      instructions: Create the workspace for \${{ inputs.customer }} on the \${{ inputs.plan }} plan and record its ID.
      outputs:
        workspace_id: { type: string, required: true }
    - id: setup
      title: Set up the account, in any order
      parallel:
        - id: sso
          title: Configure single sign-on
          actor: human
          instructions: Exchange SAML metadata with the customer's IT team and test a sign-in.
        - id: import_data
          title: Import historical data
          actor: agent
          instructions: Import the export the customer shared and report the row count.
          outputs:
            rows: { type: integer }
        - id: customer_connects
          title: Customer connects their store
          wait: { signal: store_connected, timeout: 7d }
    # The ingestion API sends this signal when the first live events arrive.
    - id: first_events
      title: First live events arrive
      wait: { signal: first_events, timeout: 3d }
    - id: go_live
      title: Approve go-live
      approval:
        prompt: Everything is set up for \${{ inputs.customer }}. Switch them to live?
        options: [go_live, hold]
__APPROVERS__    - id: welcome
      title: Send the welcome pack
      if: "\${{ steps.go_live.outputs.decision == 'go_live' }}"
      actor: agent
      instructions: Email the welcome pack and the first-week checklist to the customer's admins.
`;

export const TEMPLATES: readonly RunbookTemplate[] = [
  {
    id: 'blank',
    title: 'Blank',
    description: 'An empty runbook. Add steps in the builder.',
    who: 'Anyone',
    kinds: [],
    yaml: BLANK,
  },
  {
    id: 'post-deploy',
    title: 'Post-deploy checks',
    description:
      'An agent checks health, per-region error rates and latency, then stops the run on a failure and summarizes.',
    who: 'Agents',
    kinds: ['check', 'foreach', 'switch', 'end'],
    yaml: POST_DEPLOY,
  },
  {
    id: 'customer-onboarding',
    title: 'Customer onboarding',
    description:
      'Kickoff, setup steps in any order, signals from the customer and the API, then a go-live approval.',
    who: 'People, agents and the customer',
    kinds: ['task', 'parallel', 'wait_signal', 'approval'],
    yaml: CUSTOMER_ONBOARDING,
  },
];

export function templateById(id: string | undefined): RunbookTemplate {
  return TEMPLATES.find((t) => t.id === id) ?? (TEMPLATES[0] as RunbookTemplate);
}

/**
 * A YAML scalar on one line: plain when that is unambiguous, else double
 * quoted (a JSON string is a valid YAML double-quoted scalar).
 */
function scalar(value: string): string {
  const plain =
    /^[A-Za-z][\w .,()&/'+-]*$/.test(value) &&
    !/^(true|false|null|yes|no|on|off|~)$/i.test(value) &&
    !value.endsWith(' ');
  return plain ? value : JSON.stringify(value);
}

/** The template's YAML for a new runbook. */
export function templateSource(template: RunbookTemplate, params: TemplateParams): string {
  const approvers = params.me ? `        approvers: { users: [${scalar(params.me)}] }\n` : '';
  // Function replacers: a string replacement would read `$$`, `$&`, `$'`
  // and `$1` in a name as patterns (review round 1).
  return template.yaml
    .replace('__ID__', () => params.id)
    .replace('__TITLE__', () => scalar(params.title))
    .replace('__APPROVERS__', () => approvers);
}
