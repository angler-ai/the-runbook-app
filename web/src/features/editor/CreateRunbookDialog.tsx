// Create-runbook dialog (M5; UI-4). The creation choices live in
// NewRunbookForm; the server keeps the source verbatim as the first draft
// when the builder first saves it and never publishes on create. The slug
// is fixed at creation and must equal metadata.id when both are given
// (D23); the server decides, and its error codes are shown in plain words.
import { ApiError } from '../../api/client';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '../../ui/primitives/dialog';
import { NewRunbookForm } from '../builder/NewRunbookForm';

/** D23 and catalog errors from POST /v1/projects/{p}/runbooks, in plain words. */
export const CREATE_ERRORS: Record<string, string> = {
  'runbook.id_mismatch':
    'The slug and metadata.id in the source differ. Make them the same, or leave the slug empty.',
  'runbook.slug_required': 'Add metadata.id to the source, or fill in a slug.',
  'catalog.invalid_slug':
    'That slug is not valid. Use lowercase letters, digits and single hyphens, without “@”.',
  'catalog.runbook_slug_conflict': 'A runbook with that slug already exists in this project.',
  'catalog.runbook_exists': 'A runbook with that slug already exists in this project.',
  'spec.source_limit': 'The source is larger than the 1 MiB limit.',
  'auth.scope_missing': 'Your token cannot create runbooks: it lacks runbooks:write.',
  'auth.forbidden': 'Your project role does not allow creating runbooks.',
};

export function createErrorText(error: unknown): string {
  if (!(error instanceof ApiError))
    return 'The runbook could not be created: the server did not answer. Try again.';
  return CREATE_ERRORS[error.code] ?? `The runbook could not be created (${error.code}).`;
}

export interface CreateRunbookDialogProps {
  open: boolean;
  onOpenChange(open: boolean): void;
  projectId: string;
  projectRef: string;
}

/**
 * UI-4: "New runbook" on the list and on Home. Start blank, from a template
 * or with an AI agent; the first two open the builder with an unsaved draft.
 */
export function CreateRunbookDialog({
  open,
  onOpenChange,
  projectId,
  projectRef,
}: CreateRunbookDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>New runbook</DialogTitle>
          <DialogDescription>
            Start blank or from a template and build it in the graph, or ask your AI agent to write
            it.
          </DialogDescription>
        </DialogHeader>
        {open ? (
          <NewRunbookForm
            projectId={projectId}
            projectRef={projectRef}
            withAgent
            onDone={() => onOpenChange(false)}
          />
        ) : null}
      </DialogContent>
    </Dialog>
  );
}
