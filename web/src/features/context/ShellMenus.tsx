// UI-1: the sidebar's project switcher (Shelf's bordered workspace label) and
// account menu at its foot (appearance and sign-out, as in Shelf's settings
// popover).
import { PictureAvatar } from '../../ui/status/PictureAvatar';
import { Link } from '@tanstack/react-router';
import { Check, ChevronsUpDown, LogOut, Monitor, Moon, Settings, Sun } from 'lucide-react';
import type { ProjectView } from '../../api/hooks';
import { Popover, PopoverContent, PopoverTrigger } from '../../ui/primitives/popover';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from '../../ui/primitives/dropdown-menu';
import { cn } from '../../ui/primitives/cn';
import { useThemePreference, type ThemePreference } from '../../theme';

function initials(text: string): string {
  return (
    text
      .split(/[\s_-]+/)
      .filter(Boolean)
      .slice(0, 2)
      .map((part) => part[0]?.toUpperCase() ?? '')
      .join('') || '?'
  );
}

function Avatar({ text, className }: { text: string; className?: string }) {
  return (
    <span
      aria-hidden="true"
      className={cn(
        'flex size-7 shrink-0 items-center justify-center rounded-md border border-accent-line bg-accent-subtle text-xs font-medium text-accent',
        className,
      )}
    >
      {initials(text)}
    </span>
  );
}

export function ProjectSwitcher({
  orgTitle,
  current,
  projects,
  onSelect,
  onNewProject,
}: {
  orgTitle: string;
  current: ProjectView | undefined;
  projects: ProjectView[];
  onSelect(slug: string): void;
  onNewProject?(): void;
}) {
  const label = current?.title ?? 'No project';
  const box = (
    <>
      {current ? (
        <PictureAvatar of={{ project: current.id }} label={label} />
      ) : (
        <Avatar text={label} />
      )}
      <span className="min-w-0 flex-1 text-left">
        <span className="block truncate text-xs font-medium text-fg">{label}</span>
        <span className="block truncate text-2xs text-fg-muted">{orgTitle || 'Organization'}</span>
      </span>
    </>
  );
  const boxClasses =
    'flex w-full items-center gap-2.5 rounded-md border border-border bg-surface px-2 py-2';
  if (projects.length <= 1 && !onNewProject) {
    return (
      <div className={boxClasses} data-slot="project-switcher">
        {box}
      </div>
    );
  }
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        aria-label={`Project: ${label}. Switch project`}
        data-slot="project-switcher"
        className={cn(boxClasses, 'transition-colors duration-[120ms] hover:border-border-strong')}
      >
        {box}
        <ChevronsUpDown className="size-3.5 shrink-0 text-fg-muted" aria-hidden="true" />
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="start"
        className="w-(--radix-dropdown-menu-trigger-width) min-w-56"
      >
        <DropdownMenuLabel className="font-mono text-[10px] tracking-[0.12em] uppercase">
          Projects
        </DropdownMenuLabel>
        {projects.map((project) => (
          <DropdownMenuItem key={project.id} onSelect={() => onSelect(project.slug)}>
            <PictureAvatar
              of={{ project: project.id }}
              label={project.title}
              className="size-5 text-2xs"
            />
            <span className="min-w-0 flex-1 truncate">{project.title}</span>
            <span className="font-mono text-2xs text-fg-muted">{project.slug}</span>
            {project.id === current?.id ? (
              <Check className="text-accent" aria-label="Current project" />
            ) : null}
          </DropdownMenuItem>
        ))}
        {onNewProject ? (
          <>
            <div className="my-1 border-t border-border" />
            <DropdownMenuItem onSelect={onNewProject}>
              <span>New project...</span>
            </DropdownMenuItem>
          </>
        ) : null}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

const THEMES: { value: ThemePreference; label: string; icon: typeof Sun }[] = [
  { value: 'light', label: 'Light', icon: Sun },
  { value: 'dark', label: 'Dark', icon: Moon },
  { value: 'system', label: 'System', icon: Monitor },
];

/** Light / dark / system as three pressed-or-not buttons (Shelf's theme options). */
export function ThemeToggle({ className }: { className?: string }) {
  const [preference, setPreference] = useThemePreference();
  return (
    <div role="group" aria-label="Appearance" className={cn('flex gap-1.5', className)}>
      {THEMES.map(({ value, label, icon: Icon }) => (
        <button
          key={value}
          type="button"
          aria-pressed={preference === value}
          onClick={() => setPreference(value)}
          className={cn(
            'flex flex-1 flex-col items-center gap-1.5 rounded-md border px-2 py-2.5 text-2xs transition-colors duration-[120ms]',
            preference === value
              ? 'border-accent-line bg-accent-subtle text-accent'
              : 'border-border text-fg-muted hover:bg-hover hover:text-fg',
          )}
        >
          <Icon className="size-4" aria-hidden="true" />
          {label}
        </button>
      ))}
    </div>
  );
}

export function UserMenu({
  name,
  userId,
  role,
  signingOut,
  onSignOut,
  compact = false,
}: {
  name: string;
  /** Whose photo the avatar shows. */
  userId?: string;
  role: string | undefined;
  signingOut: boolean;
  onSignOut(): void;
  /** The avatar alone, for the collapsed sidebar's rail. */
  compact?: boolean;
}) {
  return (
    <Popover>
      <PopoverTrigger
        aria-label={`Account: ${name}`}
        title={compact ? name : undefined}
        className={
          compact
            ? 'flex size-9 items-center justify-center rounded-md transition-colors duration-[120ms] hover:bg-sidebar-hover'
            : 'flex w-full items-center gap-2.5 rounded-md px-1.5 py-1.5 text-left transition-colors duration-[120ms] hover:bg-sidebar-hover'
        }
      >
        {userId ? (
          <PictureAvatar of={{ user: userId }} label={name} />
        ) : (
          <Avatar text={name} className="rounded-full" />
        )}
        {compact ? null : (
          <>
            <span className="min-w-0 flex-1">
              <span className="block truncate text-xs font-medium text-fg">{name}</span>
              {role ? (
                <span className="block truncate text-2xs text-fg-muted capitalize">{role}</span>
              ) : null}
            </span>
            <ChevronsUpDown className="size-3.5 shrink-0 text-fg-muted" aria-hidden="true" />
          </>
        )}
      </PopoverTrigger>
      <PopoverContent side="top" align="start" className="w-64" aria-label="Account">
        <p className="font-mono text-[10px] font-medium tracking-[0.12em] text-fg-muted uppercase">
          Appearance
        </p>
        <ThemeToggle className="mt-2.5 mb-3" />
        <div className="-mx-4 border-t border-border" />
        <Link
          to="/settings"
          className="mt-2.5 flex w-full items-center gap-2.5 rounded-md px-2 py-2 text-sm text-fg hover:bg-hover"
        >
          <Settings className="size-4 text-fg-muted" aria-hidden="true" />
          Settings
        </Link>
        <Link
          to="/settings/organization"
          className="mt-1 flex w-full items-center gap-2.5 rounded-md px-2 py-2 text-sm text-fg hover:bg-hover"
        >
          <Settings className="size-4 text-fg-muted" aria-hidden="true" />
          Organization settings
        </Link>
        <button
          type="button"
          disabled={signingOut}
          onClick={onSignOut}
          className="mt-1 flex w-full items-center gap-2.5 rounded-md px-2 py-2 text-sm text-fg hover:bg-hover disabled:opacity-50"
        >
          <LogOut className="size-4 text-fg-muted" aria-hidden="true" />
          Sign out
        </button>
      </PopoverContent>
    </Popover>
  );
}
