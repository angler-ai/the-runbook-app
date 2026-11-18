// The first sign-in asks for a name (and offers a photo) when the account
// still has a placeholder one: the bootstrap "Owner" or the first part of
// an email address (2026-11-17). Asked once per person in this browser.
import { useId, useState } from 'react';
import { useMe } from '../../api/hooks';
import { Button } from '../../ui/primitives/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../../ui/primitives/dialog';
import { Input } from '../../ui/primitives/input';
import { PictureEditor } from './PictureEditor';
import { useMyPicture, useUpdateMyName } from './hooks';

const ASKED = 'rb.profile.asked.';

/** A name nobody chose: a bootstrap default or an email's first part. */
export function looksUnset(name: string): boolean {
  const n = name.trim();
  return (
    n === '' ||
    ['owner', 'administrator', 'admin', 'user'].includes(n.toLowerCase()) ||
    /^[a-z0-9._+-]+$/.test(n)
  );
}

function asked(userId: string): boolean {
  try {
    return localStorage.getItem(ASKED + userId) === '1';
  } catch {
    return false;
  }
}

function remember(userId: string): void {
  try {
    localStorage.setItem(ASKED + userId, '1');
  } catch {
    // Storage blocked: it may ask again next time.
  }
}

export function WelcomeDialog() {
  const me = useMe();
  const user = me.data?.user;
  const human = me.data?.credential.actor_kind === 'human';
  const [closed, setClosed] = useState(false);
  if (!user || !human || closed || asked(user.id) || !looksUnset(user.name)) return null;
  return (
    <WelcomeForm
      userId={user.id}
      name={user.name}
      onDone={() => {
        remember(user.id);
        setClosed(true);
      }}
    />
  );
}

function WelcomeForm({ userId, name, onDone }: { userId: string; name: string; onDone(): void }) {
  const nameId = useId();
  const [draft, setDraft] = useState('');
  const rename = useUpdateMyName();
  const picture = useMyPicture();
  const trimmed = draft.trim();
  return (
    <Dialog open onOpenChange={(open) => (open ? undefined : onDone())}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Welcome to The Runbook</DialogTitle>
          <DialogDescription>
            What should we call you? Your name and photo show on your tasks, approvals and runs.
          </DialogDescription>
        </DialogHeader>
        <form
          className="grid gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            if (!trimmed) return;
            void rename.mutateAsync(trimmed).then(onDone);
          }}
        >
          <div className="grid gap-1">
            <label htmlFor={nameId} className="text-sm font-medium text-fg">
              Your name
            </label>
            <Input
              id={nameId}
              value={draft}
              maxLength={100}
              autoComplete="name"
              onChange={(e) => setDraft(e.target.value)}
            />
          </div>
          <PictureEditor
            of={{ user: userId }}
            label={trimmed || name}
            save={(data) => picture.mutateAsync(data)}
            busy={picture.isPending}
          />
          {rename.isError ? (
            <p role="alert" className="text-xs text-danger-text">
              The name could not be saved.
            </p>
          ) : null}
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={onDone}>
              Later
            </Button>
            <Button type="submit" disabled={!trimmed || rename.isPending}>
              {rename.isPending ? 'Saving…' : 'Save'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
