import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { renderHook, waitFor } from '@testing-library/react';
import { http, HttpResponse } from 'msw';
import type { ReactNode } from 'react';
import { describe, expect, it } from 'vitest';
import { server } from '../../test/msw/server';
import { useResolvedRunbookId } from './hooks';

function wrapper({ children }: { children: ReactNode }) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
}

describe('useResolvedRunbookId', () => {
  it('takes an rb_ ID as is', () => {
    const { result } = renderHook(() => useResolvedRunbookId(undefined, 'rb_1'), { wrapper });
    expect(result.current).toMatchObject({ runbookId: 'rb_1', pending: false, missing: false });
  });

  it('stays pending while the project is unknown, never "not found"', () => {
    const { result } = renderHook(() => useResolvedRunbookId(undefined, 'deploy'), { wrapper });
    expect(result.current).toMatchObject({ pending: true, missing: false, failed: false });
  });

  it('resolves a slug, and reports a missing one', async () => {
    server.use(
      http.get('*/v1/projects/:p/runbooks/:slug', ({ params }) =>
        params.slug === 'deploy'
          ? HttpResponse.json({ runbook_id: 'rb_9', slug: 'deploy', archived: false, tags: [] })
          : HttpResponse.json(
              { status: 404, code: 'resource.not_found', title: 'resource.not_found' },
              { status: 404 },
            ),
      ),
    );
    const found = renderHook(() => useResolvedRunbookId('prj_1', 'deploy'), { wrapper });
    await waitFor(() => expect(found.result.current.runbookId).toBe('rb_9'));
    const gone = renderHook(() => useResolvedRunbookId('prj_1', 'gone'), { wrapper });
    await waitFor(() => expect(gone.result.current.missing).toBe(true));
  });

  it('reports a failed lookup as failed, not missing', async () => {
    server.use(
      http.get('*/v1/projects/:p/runbooks/:slug', () =>
        HttpResponse.json(
          { status: 503, code: 'storage.unavailable', title: 'storage.unavailable' },
          { status: 503 },
        ),
      ),
    );
    const { result } = renderHook(() => useResolvedRunbookId('prj_1', 'deploy'), { wrapper });
    await waitFor(() => expect(result.current.failed).toBe(true));
    expect(result.current.missing).toBe(false);
  });
});
