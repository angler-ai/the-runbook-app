// Command palette content (A10 wiring): recent + searchable jumps to
// runbooks and executions, start a run, My tasks, sign out.
import { useNavigate } from '@tanstack/react-router';
import { useCallback, useMemo, useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { SearchTrigger } from '../../ui/layout/AppShell';
import { CommandPalette, useCommandPaletteHotkey, type CommandPaletteItem } from '../../ui/palette';
import { signOut } from '../auth/session';

const RECENTS_KEY = 'runbook.palette.recents.v1';

export interface RecentEntry {
  kind: 'runbook' | 'execution';
  id: string;
  label: string;
  projectRef?: string;
}

export function readRecents(): RecentEntry[] {
  try {
    const raw = globalThis.sessionStorage?.getItem(RECENTS_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    return Array.isArray(parsed) ? (parsed as RecentEntry[]) : [];
  } catch {
    return [];
  }
}

export function rememberRecent(entry: RecentEntry) {
  try {
    const next = [entry, ...readRecents().filter((r) => r.id !== entry.id)].slice(0, 5);
    globalThis.sessionStorage?.setItem(RECENTS_KEY, JSON.stringify(next));
  } catch {
    // Recents are a convenience; losing them costs nothing.
  }
}

/**
 * Another dialog is open (a confirm, the start dialog, the phone drawer):
 * ⌘K must not stack the palette on top of it (U20).
 */
function anotherDialogOpen(): boolean {
  return document.querySelector('[role="dialog"], [role="alertdialog"]') !== null;
}

export interface PaletteHostProps {
  projectRef: string | undefined;
  runbooks: { runbook_id: string; slug: string; title?: string | null }[];
  executions: { id: string; title: string }[];
  onStartRun(): void;
}

export function PaletteHost({ projectRef, runbooks, executions, onStartRun }: PaletteHostProps) {
  const [open, setOpen] = useState(false);
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const setOpenByHotkey = useCallback((next: boolean | ((prev: boolean) => boolean)) => {
    setOpen((prev) => {
      const value = typeof next === 'function' ? next(prev) : next;
      return value && !prev && anotherDialogOpen() ? prev : value;
    });
  }, []);
  useCommandPaletteHotkey(open, setOpenByHotkey);
  // Read on every open (U20): a recent recorded since, or the recents a
  // sign-out cleared, would otherwise show as they were at first render.
  const recentEntries = useMemo(() => (open ? readRecents() : []), [open]);

  const items = useMemo<CommandPaletteItem[]>(() => {
    const goRunbook = (rb: { runbook_id: string; slug: string; title?: string | null }) => {
      rememberRecent({
        kind: 'runbook',
        id: rb.runbook_id,
        label: rb.title ?? rb.slug,
        projectRef,
      });
      if (projectRef) {
        void navigate({
          to: '/p/$project/runbooks/$rb/editor',
          params: { project: projectRef, rb: rb.runbook_id },
        });
      }
    };
    const goExecution = (exe: { id: string; title: string }) => {
      rememberRecent({ kind: 'execution', id: exe.id, label: exe.title, projectRef });
      void navigate({ to: '/x/$exe', params: { exe: exe.id } });
    };
    const recents: CommandPaletteItem[] = recentEntries.map((entry) => ({
      id: `recent-${entry.kind}-${entry.id}`,
      label: entry.label,
      group: 'Recent',
      hint: entry.kind,
      onSelect: () => {
        if (entry.kind === 'execution') goExecution({ id: entry.id, title: entry.label });
        else if (entry.projectRef) {
          void navigate({
            to: '/p/$project/runbooks/$rb/editor',
            params: { project: entry.projectRef, rb: entry.id },
          });
        }
      },
    }));
    return [
      ...recents,
      {
        id: 'action-search',
        label: 'Search everything',
        group: 'Actions',
        keywords: ['search', 'find', 'universal'],
        onSelect: () => void navigate({ to: '/search' }),
      },
      {
        id: 'action-start',
        label: 'Start a run',
        group: 'Actions',
        keywords: ['start', 'run', 'execute'],
        onSelect: onStartRun,
      },
      {
        id: 'action-tasks',
        label: 'Inbox (My tasks)',
        group: 'Actions',
        keywords: ['inbox', 'tasks', 'approvals'],
        onSelect: () => void navigate({ to: '/tasks' }),
      },
      {
        id: 'action-sign-out',
        label: 'Sign out',
        group: 'Actions',
        keywords: ['logout', 'revoke'],
        onSelect: () => {
          void (async () => {
            const { revoked } = await signOut();
            queryClient.clear();
            await navigate({
              to: '/sign-in',
              search: { reason: revoked ? 'signed-out' : 'revoke-failed' },
            });
          })();
        },
      },
      ...runbooks.map((rb) => ({
        id: `runbook-${rb.runbook_id}`,
        label: rb.title ?? rb.slug,
        group: 'Runbooks',
        hint: rb.slug,
        keywords: [rb.slug],
        onSelect: () => goRunbook(rb),
      })),
      ...executions.map((exe) => ({
        id: `execution-${exe.id}`,
        label: exe.title,
        group: 'Executions',
        hint: exe.id,
        keywords: [exe.id],
        onSelect: () => goExecution(exe),
      })),
    ];
  }, [executions, navigate, onStartRun, projectRef, queryClient, recentEntries, runbooks]);

  return (
    <>
      <SearchTrigger onClick={() => setOpen(true)} />
      <CommandPalette open={open} onOpenChange={setOpen} items={items} />
    </>
  );
}

/** For tests: the hotkey hook re-exported with the palette's contract. */
export { useCommandPaletteHotkey } from '../../ui/palette';
export { isInputElement } from '../../ui/palette';
