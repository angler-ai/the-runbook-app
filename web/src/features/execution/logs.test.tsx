// UI-3 review: a live refresh reads only the lines after the last one
// loaded, never every page again; copy loads the pages not read yet.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, renderHook, waitFor } from '@testing-library/react';
import { http, HttpResponse } from 'msw';
import type { ReactNode } from 'react';
import { afterEach, describe, expect, it } from 'vitest';
import { tokenStore } from '../../api/token-store';
import { server } from '../../test/msw/server';
import { useLogView } from './logs';
import { midRollout } from './fixtures/runs';

afterEach(() => tokenStore.clear());

function serveLog(total: () => number, cursors: string[]) {
  server.use(
    http.get('*/v1/step-runs/:sr/logs', ({ request }) => {
      const url = new URL(request.url);
      const cursor = Number(url.searchParams.get('cursor') ?? '0');
      const limit = Number(url.searchParams.get('limit') ?? '200');
      cursors.push(String(cursor));
      const end = Math.min(total(), cursor + limit);
      const lines = [];
      for (let seq = cursor + 1; seq <= end; seq += 1) lines.push({ seq, line: `line ${seq}` });
      return HttpResponse.json({
        lines,
        next_cursor: String(lines.at(-1)?.seq ?? cursor),
        has_more: end < total(),
      });
    }),
  );
}

function setup() {
  tokenStore.set('trb_pat_good');
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
  const section = midRollout().sections.find((s) => s.status === 'claimed')!;
  return { section, wrapper };
}

describe('a step log on the run page', () => {
  it('reads only new lines on a live refresh, not every loaded page again', async () => {
    let total = 250;
    const cursors: string[] = [];
    serveLog(() => total, cursors);
    const { section, wrapper } = setup();
    const { result, rerender } = renderHook(({ at }) => useLogView(section, at), {
      wrapper,
      initialProps: { at: 1 },
    });
    act(() => result.current.open());
    await waitFor(() => expect(result.current.lines).toHaveLength(200));
    act(() => result.current.loadMore());
    await waitFor(() => expect(result.current.lines).toHaveLength(250));
    expect(cursors).toEqual(['0', '200']);
    // The step writes one more line; the stream refreshes the document.
    total = 251;
    rerender({ at: 2 });
    await waitFor(() => expect(result.current.lines).toHaveLength(251));
    expect(result.current.lines.at(-1)).toBe('line 251');
    expect(cursors).toEqual(['0', '200', '250']);
  });

  it('copies the whole log, loading the pages not read yet', async () => {
    const cursors: string[] = [];
    serveLog(() => 450, cursors);
    const { section, wrapper } = setup();
    const { result } = renderHook(() => useLogView(section, 1), { wrapper });
    act(() => result.current.open());
    await waitFor(() => expect(result.current.lines).toHaveLength(200));
    let copied: { lines: string[]; complete: boolean } | undefined;
    await act(async () => {
      copied = await result.current.copyAll();
    });
    expect(copied?.complete).toBe(true);
    expect(copied?.lines).toHaveLength(450);
    expect(copied?.lines.at(-1)).toBe('line 450');
  });
});
