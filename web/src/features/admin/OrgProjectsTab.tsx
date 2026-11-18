// Organization settings → Projects (2026-11-17): every project with its
// image and name; owners and admins edit both here, and open the project's
// own settings for members and Slack.
import { Link } from '@tanstack/react-router';
import { Pencil } from 'lucide-react';
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

export function OrgProjectsTab({ canEdit }: { canEdit: boolean }) {
  const { projects } = useOrgContext();
  const [editing, setEditing] = useState<string | null>(null);
  const project = projects.find((p) => p.id === editing);
  return (
    <section aria-labelledby="org-projects-heading" className="grid gap-4">
      <div>
        <h2 id="org-projects-heading" className="text-sm font-semibold text-fg">
          Projects
        </h2>
        <p className="text-xs text-fg-muted">
          Each project's name and image. Members, roles and Slack are on the project's own settings.
        </p>
      </div>
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
          </li>
        ))}
      </ul>
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
