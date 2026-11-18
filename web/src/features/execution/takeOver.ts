// LANG7: a person taking over an agent step, and the gate that decides
// when to offer it. The server re-authorizes every claim; these only decide
// what to show.
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { api, type Schemas } from '../../api/client';
import type { MeView } from '../../api/hooks';
import type { DirectoryNames } from '../context/names';
import { actionId, runAction } from './actions';
import type { Capabilities } from './gating';
import { stepReadKey, useInvalidateExecution } from './hooks';
import { tabClaims, tabSessionId, type ClaimRef } from './session';

type Section = Schemas['StepSection'];

function matches(ref: Schemas['RecipientRef'], me: MeView): boolean {
  if (ref.kind === 'user') return me.user?.id === ref.reference;
  return me.teams.some((team) => team.id === ref.reference || team.slug === ref.reference);
}

/** `user:<id or name>` or `team:<id or slug>` as the runbook wrote it. */
function matchesWritten(to: string, me: MeView): boolean {
  const [kind, reference] = [to.slice(0, to.indexOf(':')), to.slice(to.indexOf(':') + 1)];
  if (!reference) return false;
  if (kind === 'user') return me.user?.id === reference || me.user?.name === reference;
  if (kind === 'team') return matches({ kind: 'team', reference }, me);
  return false;
}

/**
 * Whether a strict assignee lets this person claim the step (LANG7): the
 * resolved assignee names them or a team they are in. While it resolved to
 * nobody, the server resolves it again at claim time against the live
 * directory, so the assignee as written decides what to offer. A guidance
 * assignee (or none) limits nobody.
 */
export function strictAssigneeAllows(section: Section, me: MeView | undefined): boolean {
  const assignee = section.assignee;
  if (!assignee?.strict) return true;
  if (!me?.user) return false;
  const recipients = assignee.recipients ?? [];
  if (recipients.length > 0) return recipients.some((ref) => matches(ref, me));
  return assignee.to ? matchesWritten(assignee.to, me) : false;
}

/** Who a strict assignee names, in directory names where known. */
export function strictAssigneeLabel(
  section: Section,
  names: Pick<DirectoryNames, 'nameOf' | 'teamName'>,
): string {
  const recipients = section.assignee?.recipients ?? [];
  if (recipients.length === 0) return section.assignee?.to ?? 'someone else';
  return recipients
    .map((ref) =>
      ref.kind === 'user' ? names.nameOf(ref.reference) : names.teamName(ref.reference),
    )
    .join(', ');
}

/** "Taken over by <person> from an agent", from the run document (LANG7). */
export function takenOverText(section: Section, nameOf: (id: string) => string): string | null {
  const taken = section.taken_over;
  if (!taken) return null;
  const by =
    taken.by.kind === 'human'
      ? nameOf(taken.by.user_id)
      : taken.by.kind === 'agent'
        ? taken.by.name
        : 'the system';
  const from = taken.previous_actor_kind === 'agent' ? 'an agent' : `a ${taken.previous_actor_kind}`;
  return `Taken over by ${by} from ${from}`;
}

/**
 * Whether to offer "Take over" (LANG7): a ready `actor: agent` task, to a
 * person who may act in the project. A strict assignee must name them (or a
 * team they are in); a live agent claim is never offered, since the step is
 * then `claimed`, not `ready`. Never where a plain claim is the action (a
 * handed-off agent step) or while the run is paused: the server refuses both.
 */
export function canTakeOver(
  section: Section,
  me: MeView | undefined,
  capabilities: Pick<Capabilities, 'canAct'>,
  context: { paused?: boolean; canClaim?: boolean } = {},
): boolean {
  if (!me?.user || !capabilities.canAct) return false;
  if (context.paused || context.canClaim) return false;
  if (section.task_actor !== 'agent' || section.status !== 'ready') return false;
  return strictAssigneeAllows(section, me);
}

/** Claim an agent step as this person, with `take_over: true`. */
export function useTakeOverStep(executionId: string) {
  const queryClient = useQueryClient();
  const invalidate = useInvalidateExecution(executionId);
  return useMutation({
    mutationFn: async ({ stepRunId }: { stepRunId: string }) => {
      const session = tabSessionId();
      const claim = await runAction(actionId('take-over', { stepRunId, session }), async (key) => {
        const { data } = await api.POST('/v1/step-runs/{sr}/claim', {
          params: { path: { sr: stepRunId }, header: { 'Idempotency-Key': key } },
          body: { session_id: session, actor: { kind: 'human' }, take_over: true },
        });
        return data as ClaimRef;
      });
      tabClaims.set(stepRunId, claim);
      return claim;
    },
    onSettled: (_data, _error, variables) => {
      invalidate();
      void queryClient.invalidateQueries({ queryKey: stepReadKey(variables.stepRunId) });
    },
  });
}
