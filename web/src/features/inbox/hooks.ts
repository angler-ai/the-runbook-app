// My tasks inbox source (B8f): polls every 30 seconds and on window
// focus (the app query client refetches on focus). Shared by the tasks
// page and the sidebar count so both read one cache entry.
//
// U19: the inbox follows `next_cursor` to the last page, so the list and
// the sidebar count cover every open task, not the first page of 50. A
// runaway inbox stops after INBOX_MAX_PAGES; the result then keeps the
// unread cursor (`next_cursor`) and `truncated`, and the page says so.
import { useQuery, type QueryClient } from '@tanstack/react-query';
import { api, type Schemas } from '../../api/client';
import { queryKeys } from '../../api/query-keys';
import { signedIn } from '../../api/hooks';

export const INBOX_POLL_MS = 30_000;
/** At the server's default 50 a page, 1,000 open tasks. */
export const INBOX_MAX_PAGES = 20;

export type InboxTask = Schemas['InboxTaskView'];

export interface Inbox {
  items: InboxTask[];
  /** Set only when INBOX_MAX_PAGES stopped the read before the end. */
  next_cursor: string | null;
  truncated: boolean;
}

export async function readInbox(): Promise<Inbox> {
  const items: InboxTask[] = [];
  let cursor: string | undefined;
  for (let page = 0; page < INBOX_MAX_PAGES; page += 1) {
    const { data } = await api.GET('/v1/me/tasks', {
      params: { query: cursor ? { cursor } : {} },
    });
    const result = data as Schemas['InboxPage'];
    items.push(...result.items);
    if (!result.next_cursor) return { items, next_cursor: null, truncated: false };
    cursor = result.next_cursor;
  }
  return { items, next_cursor: cursor ?? null, truncated: true };
}

/** Every inbox read (the list and the sidebar count share one entry under this). */
const INBOX_PREFIX = ['me', 'tasks'] as const;

/**
 * Re-read the inbox now (walkthrough #10): after this person decides,
 * claims, reports or releases, and on live events for a run they are
 * viewing, a task may have opened or closed; the 30 s poll is too slow.
 */
export function invalidateInbox(queryClient: QueryClient): void {
  void queryClient.invalidateQueries({ queryKey: INBOX_PREFIX });
}

export function useInbox() {
  return useQuery({
    queryKey: queryKeys.myTasks(),
    // Any signed-in person: a pasted token or an email/Google session (whose
    // cookie the browser sends). Gating on the token alone left the inbox
    // unloaded, a blank page and a 0 count, for every session sign-in.
    enabled: signedIn(),
    refetchInterval: INBOX_POLL_MS,
    refetchOnWindowFocus: true,
    queryFn: readInbox,
  });
}
