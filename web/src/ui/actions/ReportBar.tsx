import { useId, useState } from 'react';
import { Button } from '../primitives/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../primitives/dialog';
import { Textarea } from '../primitives/textarea';
import { Label } from '../primitives/label';
import { cn } from '../primitives/cn';

export type ReportAction = 'done' | 'failed' | 'blocked' | 'skip';

export interface ReportPayload {
  action: ReportAction;
  summary?: string;
}

export interface ReportBarProps {
  allowSkip?: boolean;
  disabled?: boolean;
  /**
   * False when another control reports success (a step with outputs:
   * the outputs form's own Done sends them with the summary).
   */
  showDone?: boolean;
  /** A summary already written beside the bar; it prefills the reason dialog. */
  summary?: string;
  onReport?: (payload: ReportPayload) => void;
  className?: string;
}

type ReasonAction = 'failed' | 'blocked' | 'skip';

const DIALOG: Record<ReasonAction, { title: string; verb: string; placeholder: string }> = {
  failed: {
    title: 'Report Step as Failed',
    verb: 'failed',
    placeholder: 'What went wrong or caused the blocker?',
  },
  blocked: {
    title: 'Report Step as Blocked',
    verb: 'blocked',
    placeholder: 'What went wrong or caused the blocker?',
  },
  skip: {
    title: 'Skip This Step',
    verb: 'skipped',
    placeholder: 'Why is this step being skipped?',
  },
};

export function ReportBar({
  allowSkip = false,
  disabled = false,
  showDone = true,
  summary: writtenSummary = '',
  onReport,
  className,
}: ReportBarProps) {
  const [modalAction, setModalAction] = useState<ReasonAction | null>(null);
  const [summary, setSummary] = useState('');
  const [error, setError] = useState<string | null>(null);
  const summaryId = useId();
  const errorId = `${summaryId}-error`;

  const handleDone = () => {
    onReport?.({
      action: 'done',
      ...(writtenSummary.trim() ? { summary: writtenSummary.trim() } : {}),
    });
  };

  // Failed, Blocked and Skip each need a reason (the server refuses a skip
  // without one: step.skip_reason_required).
  const handleOpenModal = (action: ReasonAction) => {
    setModalAction(action);
    setSummary(writtenSummary);
    setError(null);
  };

  const handleModalSubmit = () => {
    const trimmed = summary.trim();
    if (!trimmed) {
      setError('A summary is required for this report action.');
      return;
    }

    if (modalAction) {
      onReport?.({ action: modalAction, summary: trimmed });
      setModalAction(null);
    }
  };

  return (
    <>
      <div
        data-slot="report-bar"
        className={cn(
          'flex flex-wrap items-center gap-2 p-3 bg-surface border border-border rounded-lg',
          className,
        )}
      >
        {/* Done */}
        {showDone && (
          <Button variant="primary" size="default" disabled={disabled} onClick={handleDone}>
            Done
          </Button>
        )}

        {/* Failed */}
        <Button
          variant="destructive"
          size="default"
          disabled={disabled}
          onClick={() => handleOpenModal('failed')}
        >
          Failed
        </Button>

        {/* Blocked */}
        <Button
          variant="secondary"
          size="default"
          disabled={disabled}
          onClick={() => handleOpenModal('blocked')}
        >
          Blocked
        </Button>

        {/* Skip (only when allowSkip is set) */}
        {allowSkip && (
          <Button
            variant="ghost"
            size="default"
            disabled={disabled}
            onClick={() => handleOpenModal('skip')}
          >
            Skip
          </Button>
        )}
      </div>

      {/* Required summary dialog for Failed and Blocked */}
      <Dialog
        open={modalAction !== null}
        onOpenChange={(open) => {
          if (!open) setModalAction(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{DIALOG[modalAction ?? 'failed'].title}</DialogTitle>
            <DialogDescription>
              Please provide a summary describing why this step was marked as{' '}
              {DIALOG[modalAction ?? 'failed'].verb}. A summary is required.
            </DialogDescription>
          </DialogHeader>

          <div className="grid gap-2 py-2">
            <Label htmlFor={summaryId} className="text-xs font-medium text-fg">
              Summary{' '}
              <span className="text-danger" aria-hidden="true">
                *
              </span>
            </Label>
            <Textarea
              id={summaryId}
              required
              aria-invalid={error ? true : undefined}
              aria-describedby={error ? errorId : undefined}
              placeholder={DIALOG[modalAction ?? 'failed'].placeholder}
              value={summary}
              onChange={(e) => {
                setSummary(e.target.value);
                if (error) setError(null);
              }}
              rows={3}
            />
            {error && (
              <p id={errorId} role="alert" className="text-2xs text-danger">
                {error}
              </p>
            )}
          </div>

          <DialogFooter>
            <Button variant="secondary" onClick={() => setModalAction(null)}>
              Cancel
            </Button>
            <Button
              variant={modalAction === 'failed' ? 'destructive' : 'primary'}
              disabled={!summary.trim()}
              onClick={handleModalSubmit}
            >
              Submit report
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
