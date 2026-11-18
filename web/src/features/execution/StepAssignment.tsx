// LANG7: who a step is assigned to when the assignee is strict, and who took
// over an agent step. Renders nothing for other steps.
import type { Schemas } from '../../api/client';
import type { NameOf } from '../../ui/execution/names';

export function StepAssignment({
  section,
  nameOf,
}: {
  section: Schemas['StepSection'];
  nameOf: NameOf;
}) {
  const taken = section.taken_over;
  const strict = section.assignee?.strict ? section.assignee : null;
  if (!taken && !strict) return null;
  return (
    <div
      className="flex flex-wrap items-center gap-3 text-xs text-fg-muted"
      data-testid={`assignment-${section.step_run_id}`}
    >
      {taken ? (
        <span>
          Taken over by {taken.by.kind === 'human' ? nameOf(taken.by.user_id) : taken.by.kind} from
          an {taken.previous_actor_kind}
        </span>
      ) : null}
      {strict ? (
        <span>
          Strict assignee: <span className="font-mono">{strict.to ?? 'unresolved'}</span> (only they
          can claim it)
          {strict.routing_error ? ` · no one can claim it (${strict.routing_error})` : null}
        </span>
      ) : null}
    </div>
  );
}
