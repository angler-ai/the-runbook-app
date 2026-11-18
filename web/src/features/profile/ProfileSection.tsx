// Your profile (2026-11-17): the name and photo everyone sees on your
// tasks, approvals and runs.
import { useId, useState } from 'react';
import { useMe } from '../../api/hooks';
import { Button } from '../../ui/primitives/button';
import { Input } from '../../ui/primitives/input';
import { PictureEditor } from './PictureEditor';
import { useMyPicture, useUpdateMyName } from './hooks';

export function ProfileSection() {
  const me = useMe();
  const user = me.data?.user;
  if (!user) return null;
  return <ProfileForm key={user.id} userId={user.id} name={user.name} />;
}

function ProfileForm({ userId, name }: { userId: string; name: string }) {
  const nameId = useId();
  const [draft, setDraft] = useState(name);
  const [saved, setSaved] = useState(false);
  const rename = useUpdateMyName();
  const picture = useMyPicture();
  const trimmed = draft.trim();
  const invalid = trimmed.length === 0 || trimmed.length > 100;

  return (
    <section aria-labelledby={`${nameId}-heading`} className="grid gap-4">
      <div>
        <h2 id={`${nameId}-heading`} className="text-base font-semibold text-fg">
          Profile
        </h2>
        <p className="text-xs text-fg-muted">
          Your name and photo, as people see them on tasks, approvals and runs.
        </p>
      </div>
      <PictureEditor
        of={{ user: userId }}
        label={trimmed || name}
        save={(data) => picture.mutateAsync(data)}
        busy={picture.isPending}
      />
      <form
        className="flex flex-wrap items-end gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          if (invalid) return;
          setSaved(false);
          void rename.mutateAsync(trimmed).then(() => setSaved(true));
        }}
      >
        <div className="grid min-w-64 flex-1 gap-1 sm:max-w-sm">
          <label htmlFor={nameId} className="text-sm font-medium text-fg">
            Name
          </label>
          <Input
            id={nameId}
            value={draft}
            maxLength={100}
            autoComplete="name"
            onChange={(e) => {
              setDraft(e.target.value);
              setSaved(false);
            }}
          />
        </div>
        <Button type="submit" disabled={invalid || trimmed === name || rename.isPending}>
          {rename.isPending ? 'Saving…' : 'Save name'}
        </Button>
        <p role="status" className="basis-full text-xs text-fg-muted">
          {rename.isError ? 'The name could not be saved.' : saved ? 'Saved.' : ''}
        </p>
      </form>
    </section>
  );
}
