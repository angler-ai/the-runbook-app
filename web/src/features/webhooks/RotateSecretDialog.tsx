import { useState } from 'react';
import { Copy, Check, KeyRound, AlertTriangle, Loader2 } from 'lucide-react';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from '../../ui/primitives/dialog';
import { Button } from '../../ui/primitives/button';
import { Input } from '../../ui/primitives/input';
import { Label } from '../../ui/primitives/label';
import { friendlyWebhookError, useRotateWebhookSecret, type WebhookView } from './hooks';

export interface RotateSecretDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  projectId: string;
  webhook: WebhookView | null;
}

export function RotateSecretDialog({
  open,
  onOpenChange,
  projectId,
  webhook,
}: RotateSecretDialogProps) {
  const [error, setError] = useState<string | null>(null);
  const [rotatedSecret, setRotatedSecret] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const rotateMutation = useRotateWebhookSecret(projectId);

  function resetState() {
    setError(null);
    setRotatedSecret(null);
    setCopied(false);
  }

  function handleOpenChange(nextOpen: boolean) {
    if (!nextOpen) {
      resetState();
    }
    onOpenChange(nextOpen);
  }

  async function handleRotate() {
    if (!webhook) return;
    setError(null);
    try {
      const res = await rotateMutation.mutateAsync(webhook.id);
      setRotatedSecret(res.secret);
    } catch (err) {
      setError(friendlyWebhookError(err));
    }
  }

  async function handleCopy() {
    if (!rotatedSecret) return;
    try {
      await navigator.clipboard.writeText(rotatedSecret);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // Ignore
    }
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{rotatedSecret ? 'Secret rotated' : 'Rotate signing secret'}</DialogTitle>
          <DialogDescription>
            {rotatedSecret
              ? 'Save your new signing secret now. It will not be shown again.'
              : `Issue a new signing secret for "${webhook?.name ?? 'this webhook'}".`}
          </DialogDescription>
        </DialogHeader>

        {rotatedSecret ? (
          <div className="space-y-4 py-2">
            <div className="rounded-lg border border-verdict-warn-border bg-verdict-warn-bg p-3 text-xs text-verdict-warn-fg flex items-start gap-2">
              <KeyRound className="size-4 shrink-0 mt-0.5" aria-hidden="true" />
              <span>
                Copy your new signing secret now. For 24 hours, deliveries will carry signatures for
                both secrets.
              </span>
            </div>

            <div className="space-y-1.5">
              <Label htmlFor="rotated-signing-secret" className="text-xs">
                New Signing Secret
              </Label>
              <div className="flex items-center gap-2">
                <Input
                  id="rotated-signing-secret"
                  readOnly
                  value={rotatedSecret}
                  className="font-mono text-xs select-all bg-surface-raised"
                />
                <Button
                  size="sm"
                  variant="secondary"
                  onClick={() => void handleCopy()}
                  aria-label="Copy new signing secret"
                >
                  {copied ? (
                    <Check className="size-3.5 text-verdict-pass-fg" aria-hidden="true" />
                  ) : (
                    <Copy className="size-3.5" aria-hidden="true" />
                  )}
                  <span>{copied ? 'Copied' : 'Copy'}</span>
                </Button>
              </div>
            </div>

            <DialogFooter>
              <Button onClick={() => handleOpenChange(false)}>Done</Button>
            </DialogFooter>
          </div>
        ) : (
          <div className="space-y-4 py-2">
            {error ? (
              <p role="alert" className="text-xs text-verdict-fail-fg font-medium">
                {error}
              </p>
            ) : null}

            <div className="rounded-lg border border-verdict-warn-border bg-verdict-warn-bg p-3 text-xs text-verdict-warn-fg flex items-start gap-2">
              <AlertTriangle className="size-4 shrink-0 mt-0.5" aria-hidden="true" />
              <div className="space-y-1">
                <p className="font-semibold">Dual-signing transition period</p>
                <p>
                  A new secret will be generated. The server will dual-sign every webhook payload
                  with both the old and new secrets for 24 hours to give your receiving endpoint
                  time to update.
                </p>
              </div>
            </div>

            <DialogFooter>
              <Button
                type="button"
                variant="ghost"
                onClick={() => handleOpenChange(false)}
                disabled={rotateMutation.isPending}
              >
                Cancel
              </Button>
              <Button
                variant="destructive"
                onClick={() => void handleRotate()}
                disabled={rotateMutation.isPending}
              >
                {rotateMutation.isPending ? (
                  <>
                    <Loader2 className="size-3.5 animate-spin mr-1.5" aria-hidden="true" />
                    <span>Rotating…</span>
                  </>
                ) : (
                  'Rotate secret'
                )}
              </Button>
            </DialogFooter>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
