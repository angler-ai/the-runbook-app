import type { components } from '../../api/schema';
import { cn } from '../primitives/cn';
import { PictureAvatar } from './PictureAvatar';
import { Bot, Settings } from 'lucide-react';

export type ActorRef = components['schemas']['ActorRef'];

export interface ActorBadgeProps {
  actor: ActorRef;
  /** Optional human-readable name: a person's (from the directory), or a
   * system actor's (`Schedule nightly`, `Webhook on_deploy`). */
  name?: string;
  className?: string;
}

export function ActorBadge({ actor, name, className }: ActorBadgeProps) {
  if (!actor) return null;

  if (actor.kind === 'human') {
    const displayName = name || actor.user_id;

    return (
      <span
        data-slot="actor-badge"
        data-actor-kind="human"
        className={cn('inline-flex items-center gap-1.5 text-xs text-fg', className)}
      >
        <PictureAvatar
          of={{ user: actor.user_id }}
          label={displayName}
          className="size-5 text-2xs"
        />
        <span className="font-sans text-fg truncate">{displayName}</span>
      </span>
    );
  }

  if (actor.kind === 'agent') {
    const label = actor.model ? `${actor.name} · ${actor.model}` : actor.name;

    return (
      <span
        data-slot="actor-badge"
        data-actor-kind="agent"
        className={cn('inline-flex items-center gap-1.5 text-xs text-fg', className)}
      >
        <Bot className="size-3.5 shrink-0 text-fg-muted" aria-hidden="true" />
        <span className="font-mono text-fg">{label}</span>
      </span>
    );
  }

  // System
  return (
    <span
      data-slot="actor-badge"
      data-actor-kind="system"
      className={cn('inline-flex items-center gap-1.5 text-xs text-fg', className)}
    >
      <Settings className="size-3.5 shrink-0 text-fg-muted" aria-hidden="true" />
      <span className="font-sans text-fg">{name || 'System policy'}</span>
    </span>
  );
}
