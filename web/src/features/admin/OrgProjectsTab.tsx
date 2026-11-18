// Organization settings → Projects (2026-11-17): every project with its
// image and name; owners and admins create projects, edit names and images,
// archive (hidden, read-only, everything kept) and restore them here, and
// open a project's own settings for members and Slack.
import { Link } from '@tanstack/react-router';
import { Archive, ArchiveRestore, Pencil, Plus } from 'lucide-react';
import { useState } from 'react';
import { Button } from '../../ui/primitives/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '../../ui/primitives/dialog';
import { PictureAvatar } from '../../ui/status/PictureAvatar';
import { useOrgContext } from '../context/OrgContext';
import { ProjectProfileForm } from '../profile/ProjectProfileForm';
import { CreateProjectDialog } from './CreateProjectDialog';
import { useSetProjectArchived } from './hooks';

export function OrgProjectsTab({ canEdit }: { canEdit: boolean }) {
  const { org, projects, archivedProjects } = useOrgContext();
  const [editing, setEditing] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const setArchived = useSetProjectArchived(org?.id);
  const archive = (id: string, title: string, archived: boolean) => {
    if (
      archived &&
      !globalThis.confirm(
        `Archive ${title}? It disappears from the app and becomes read-only: no new runs, edits, schedules or webhook triggers. Its runbooks and runs are kept, and you can restore it here.`,
      )
    )
      return;
    setError(null);
    setArchived.mutate(
      { projectId: id, archived },
      {
        onError: () =>
          setError(
            archived ? 'The project could not be archived.' : 'The project could not be restored.',
          ),
      },
    );
  };
  const project = projects.find((p) => p.id === editing);
  return (
    <section aria-labelledby="org-projects-heading" className="grid gap-4">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 id="org-projects-heading" className="text-sm font-semibold text-fg">
            Projects
          </h2>
          <p className="text-xs text-fg-muted">
            Each project's name and image. Members, roles and Slack are on the project's own
            settings.
          </p>
        </div>
        {canEdit ? (
          <Button type="button" size="sm" onClick={() => setCreating(true)}>
            <Plus className="size-3.5" aria-hidden="true" />
            New project
          </Button>
        ) : null}
      </div>
      {error ? <p role="alert">{error}</p> : null}
      <ul className="grid gap-2">
        {projects.map((p) => (
          <li
            key={p.id}
            className="flex flex-wrap items-center gap-3 rounded-lg border border-border bg-surface px-4 py-3"
          >
            <PictureAvatar of={{ project: p.id }} label={p.title} className="size-9" />
            <div className="min-w-0 flex-1">
              <p className="truncate text-sm font-medium text-fg">{p.title}</p>
              <p className="truncate font-mono text-2xs text-fg-muted">{p.slug}</p>
            </div>
            <Link
              to="/p/$project/settings"
              params={{ project: p.slug }}
              className="text-xs font-medium text-accent hover:underline"
            >
              Members and settings
            </Link>
            {canEdit ? (
              <Button
                type="button"
                variant="secondary"
                size="sm"
                onClick={() => setEditing(p.id)}
                aria-label={`Edit ${p.title}`}
              >
                <Pencil className="size-3.5" aria-hidden="true" />
                Edit
              </Button>
            ) : null}
            {canEdit ? (
              <Button
                type="button"
                variant="secondary"
                size="sm"
                disabled={setArchived.isPending}
                onClick={() => archive(p.id, p.title, true)}
                aria-label={`Archive ${p.title}`}
              >
                <Archive className="size-3.5" aria-hidden="true" />
                Archive
              </Button>
            ) : null}
          </li>
        ))}
      </ul>
      {archivedProjects.length > 0 ? (
        <section aria-labelledby="org-archived-projects-heading" className="grid gap-2">
          <h3 id="org-archived-projects-heading" className="text-xs font-semibold text-fg-muted">
            Archived ({archivedProjects.length})
          </h3>
          <ul className="grid gap-2">
            {archivedProjects.map((p) => (
              <li
                key={p.id}
                className="flex flex-wrap items-center gap-3 rounded-lg border border-dashed border-border px-4 py-3"
              >
                <PictureAvatar
                  of={{ project: p.id }}
                  label={p.title}
                  className="size-9 opacity-60"
                />
                <div className="min-w-0 flex-1">
                  <p className="truncate text-sm font-medium text-fg-muted">{p.title}</p>
                  <p className="truncate font-mono text-2xs text-fg-muted">{p.slug}</p>
                </div>
                {canEdit ? (
                  <Button
                    type="button"
                    variant="secondary"
                    size="sm"
                    disabled={setArchived.isPending}
                    onClick={() => archive(p.id, p.title, false)}
                    aria-label={`Restore ${p.title}`}
                  >
                    <ArchiveRestore className="size-3.5" aria-hidden="true" />
                    Restore
                  </Button>
                ) : null}
              </li>
            ))}
          </ul>
        </section>
      ) : null}
      <CreateProjectDialog open={creating} onOpenChange={setCreating} />
      <Dialog
        open={Boolean(project)}
        onOpenChange={(open) => (open ? undefined : setEditing(null))}
      >
        <DialogContent className="sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>{project ? `Edit ${project.title}` : 'Edit project'}</DialogTitle>
            <DialogDescription>
              The project's name and image, as everyone sees them.
            </DialogDescription>
          </DialogHeader>
          {project ? (
            <ProjectProfileForm
              key={project.id + project.title}
              projectId={project.id}
              slug={project.slug}
              title={project.title}
              canEdit={canEdit}
            />
          ) : null}
        </DialogContent>
      </Dialog>
    </section>
  );
}
