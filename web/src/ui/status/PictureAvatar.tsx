// A person's photo or a project's image (2026-11-17), with initials when
// there is none (or it can't load: a token sign-in can't send its token
// with an <img>). After an upload, bumpPictures() reloads every one.
import { useState, useSyncExternalStore } from 'react';
import { cn } from '../primitives/cn';

let version = 0;
const listeners = new Set<() => void>();

/** Reload pictures everywhere (after an upload or removal). */
export function bumpPictures(): void {
  version += 1;
  for (const listener of listeners) listener();
}

function usePictureVersion(): number {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => version,
    () => version,
  );
}

export function userPictureUrl(userId: string, v: number): string {
  return `/v1/users/${encodeURIComponent(userId)}/picture${v ? `?v=${v}` : ''}`;
}

export function projectPictureUrl(project: string, v: number): string {
  return `/v1/projects/${encodeURIComponent(project)}/picture${v ? `?v=${v}` : ''}`;
}

export function initials(text: string): string {
  return (
    text
      .split(/[\s._-]+/)
      .filter(Boolean)
      .slice(0, 2)
      .map((part) => part.charAt(0).toUpperCase())
      .join('') || '?'
  );
}

export interface PictureAvatarProps {
  /** Whose picture: a person (user ID) or a project (ID or slug). */
  of: { user: string } | { project: string };
  /** The name its initials come from. */
  label: string;
  className?: string;
  /** Round for people, rounded square for projects. */
  shape?: 'circle' | 'square';
}

export function PictureAvatar({ of, label, className, shape }: PictureAvatarProps) {
  const v = usePictureVersion();
  const src = 'user' in of ? userPictureUrl(of.user, v) : projectPictureUrl(of.project, v);
  const [failed, setFailed] = useState<string | null>(null);
  const round = (shape ?? ('user' in of ? 'circle' : 'square')) === 'circle';
  const box = cn(
    'flex size-7 shrink-0 items-center justify-center overflow-hidden',
    round ? 'rounded-full' : 'rounded-md',
    className,
  );
  if (failed === src) {
    return (
      <span
        aria-hidden="true"
        data-slot="picture-avatar"
        className={cn(
          box,
          'border border-accent-line bg-accent-subtle text-xs font-medium text-accent',
        )}
      >
        {initials(label)}
      </span>
    );
  }
  return (
    <img
      src={src}
      alt=""
      aria-hidden="true"
      data-slot="picture-avatar"
      loading="lazy"
      referrerPolicy="no-referrer"
      className={cn(box, 'bg-surface-raised object-cover')}
      onError={() => setFailed(src)}
    />
  );
}
