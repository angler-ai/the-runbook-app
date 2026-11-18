// A project's name and image (2026-11-17), for project admins: on the
// project's settings page and in the organization's Projects tab.
import { useId, useState } from 'react';
import { Button } from '../../ui/primitives/button';
import { Input } from '../../ui/primitives/input';
import { PictureEditor } from './PictureEditor';
import { useProjectPicture, useUpdateProject } from './hooks';

export function ProjectProfileForm({
  projectId,
  slug,
  title,
  canEdit,
}: {
  projectId: string;
  slug: string;
  title: string;
  canEdit: boolean;
}) {
  const nameId = useId();
  const [draft, setDraft] = useState(title);
  const [saved, setSaved] = useState(false);
  const rename = useUpdateProject(projectId);
  const picture = useProjectPicture(projectId);
  const trimmed = draft.trim();
  const invalid = trimmed.length === 0 || trimmed.length > 100;
  return (
    <div className="grid gap-4">
      <PictureEditor
        of={{ project: projectId }}
        label={trimmed || title}
        save={(data) => picture.mutateAsync(data)}
        busy={picture.isPending}
        disabled={!canEdit}
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
            Project name
          </label>
          <Input
            id={nameId}
            value={draft}
            maxLength={100}
            disabled={!canEdit}
            onChange={(e) => {
              setDraft(e.target.value);
              setSaved(false);
            }}
          />
          <p className="text-xs text-fg-muted">
            Its address stays <code className="font-mono">{slug}</code>.
          </p>
        </div>
        <Button
          type="submit"
          disabled={!canEdit || invalid || trimmed === title || rename.isPending}
        >
          {rename.isPending ? 'Saving…' : 'Save name'}
        </Button>
        <p role="status" className="basis-full text-xs text-fg-muted">
          {rename.isError ? 'The name could not be saved.' : saved ? 'Saved.' : ''}
        </p>
      </form>
    </div>
  );
}
