import { defineGallery } from '../../gallery/registry';
import { ActorBadge } from './ActorBadge';
import { AmendedTag } from './AmendedTag';
import { FollowupTags } from './FollowupTags';
import { MonoRef } from './MonoRef';
import { StatusPill } from './StatusPill';
import { StaleTag } from './StaleTag';
import { VerdictTag } from './VerdictTag';
import type { AnyStatus } from './StatusPill';

const allStatuses: AnyStatus[] = [
  'queued',
  'pending',
  'ready',
  'claimed',
  'running',
  'waiting',
  'paused',
  'succeeded',
  'failed',
  'cancelled',
  'skipped',
  'timed_out',
  'expired',
];

export default [
  defineGallery({
    id: 'status-pill',
    group: 'Status',
    title: 'StatusPill',
    examples: [
      {
        name: 'Every status',
        render: () => (
          <div className="flex flex-wrap gap-2">
            {allStatuses.map((st) => (
              <StatusPill key={st} status={st} />
            ))}
          </div>
        ),
      },
      {
        name: 'Custom labels',
        render: () => (
          <div className="flex flex-wrap gap-2">
            <StatusPill status="running" label="Executing deployment" />
            <StatusPill status="failed" label="Build failed" />
          </div>
        ),
      },
    ],
  }),
  defineGallery({
    id: 'status-verdict',
    group: 'Status',
    title: 'VerdictTag',
    examples: [
      {
        name: 'Pass, warn, fail, and none',
        render: () => (
          <div className="flex flex-wrap items-center gap-2">
            <VerdictTag verdict="pass" />
            <VerdictTag verdict="warn" />
            <VerdictTag verdict="fail" />
            <VerdictTag verdict={null} />
          </div>
        ),
      },
    ],
  }),
  defineGallery({
    id: 'status-tags',
    group: 'Status',
    title: 'Amended & Stale Tags',
    examples: [
      {
        name: 'Tags',
        render: () => (
          <div className="flex flex-wrap items-center gap-2">
            <AmendedTag />
            <StaleTag />
            <FollowupTags
              followup={{
                reminders_sent: 2,
                escalated: true,
                escalated_to: [{ kind: 'team', reference: 'oncall' }],
              }}
            />
          </div>
        ),
      },
    ],
  }),
  defineGallery({
    id: 'status-actor',
    group: 'Status',
    title: 'ActorBadge',
    examples: [
      {
        name: 'Human, agent, and system policy',
        render: () => (
          <div className="flex flex-col gap-3">
            <ActorBadge actor={{ kind: 'human', user_id: 'usr_alice' }} name="Alice Smith" />
            <ActorBadge actor={{ kind: 'human', user_id: 'usr_bob' }} />
            <ActorBadge
              actor={{
                kind: 'agent',
                name: 'claude-code',
                model: 'opus',
                principal_id: 'prn_agent_1',
                session_id: 'ses_1',
              }}
            />
            <ActorBadge
              actor={{
                kind: 'agent',
                name: 'ops-worker',
                model: null,
                principal_id: 'prn_agent_2',
                session_id: 'ses_2',
              }}
            />
            <ActorBadge actor={{ kind: 'system' }} />
          </div>
        ),
      },
    ],
  }),
  defineGallery({
    id: 'status-monoref',
    group: 'Status',
    title: 'MonoRef',
    examples: [
      {
        name: 'Identifiers and versions',
        render: () => (
          <div className="flex flex-col gap-2">
            <div className="flex items-center gap-2 text-sm text-fg">
              <span>Execution ID:</span>
              <MonoRef>exe_01HX4A987BCDEF1234567890</MonoRef>
            </div>
            <div className="flex items-center gap-2 text-sm text-fg">
              <span>Step run:</span>
              <MonoRef>sr_01HX4A987BCDEF1234567891</MonoRef>
            </div>
            <div className="flex items-center gap-2 text-sm text-fg">
              <span>Version:</span>
              <MonoRef>post-deploy@3</MonoRef>
            </div>
          </div>
        ),
      },
    ],
  }),
];
