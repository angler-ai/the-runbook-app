import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it, vi } from 'vitest';
import { expectNoAxeViolations } from '../../test/axe';
import { server } from '../../test/msw/server';
import { RunbookArchivesTab } from './RunbookArchivesTab';
import type { ArchiveView } from './hooks';

const RB = 'rb_test';

const sampleArchives: ArchiveView[] = [
  {
    date: '2026-11-14',
    run_count: 5,
    size_bytes: 1048576, // 1.0 MB
    sha256: 'a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2',
    written_at_ms: Date.now() - 3600000,
    expires_at_ms: null,
    stale: false,
  },
  {
    date: '2026-11-13',
    run_count: 1,
    size_bytes: 2048,
    sha256: 'b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3',
    written_at_ms: Date.now() - 90000000,
    expires_at_ms: null,
    stale: true,
  },
];

function renderTab() {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
    },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <RunbookArchivesTab runbookId={RB} />
    </QueryClientProvider>,
  );
}

describe('RunbookArchivesTab', () => {
  it('renders daily archives list, passing axe check', async () => {
    server.use(
      http.get('*/v1/runbooks/:rb/archives', () =>
        HttpResponse.json({
          items: sampleArchives,
        }),
      ),
    );

    const { container } = renderTab();

    await waitFor(() => {
      expect(screen.getByText('2026-11-14')).toBeInTheDocument();
    });

    expect(screen.getByText('5 runs')).toBeInTheDocument();
    expect(screen.getByText('1.0 MB')).toBeInTheDocument();
    expect(screen.getByText('Rewrite pending')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Download archive for 2026-11-14/i })).toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('renders empty state when there are no archives, passing axe check', async () => {
    server.use(
      http.get('*/v1/runbooks/:rb/archives', () =>
        HttpResponse.json({
          items: [],
        }),
      ),
    );

    const { container } = renderTab();

    await waitFor(() => {
      expect(screen.getByText('No daily archives')).toBeInTheDocument();
    });

    await expectNoAxeViolations(container);
  });

  it('handles error state with retry', async () => {
    let callCount = 0;
    server.use(
      http.get('*/v1/runbooks/:rb/archives', () => {
        callCount++;
        if (callCount === 1) {
          return new HttpResponse(null, { status: 500 });
        }
        return HttpResponse.json({ items: sampleArchives });
      }),
    );

    renderTab();

    await waitFor(() => {
      expect(screen.getByText('Could not load daily archives')).toBeInTheDocument();
    });

    const retryBtn = screen.getByRole('button', { name: /retry/i });
    await userEvent.click(retryBtn);

    await waitFor(() => {
      expect(screen.getByText('2026-11-14')).toBeInTheDocument();
    });
  });

  it('downloads an archive on click', async () => {
    let downloadedDate: string | null = null;
    server.use(
      http.get('*/v1/runbooks/:rb/archives', () =>
        HttpResponse.json({
          items: sampleArchives,
        }),
      ),
      http.get('*/v1/runbooks/:rb/archives/:date', ({ params }) => {
        downloadedDate = params.date as string;
        return new HttpResponse(new Blob(['fake zip bytes']), {
          headers: { 'Content-Type': 'application/zip' },
        });
      }),
    );

    // Mock createObjectURL & revokeObjectURL
    const createObjectURL = vi.fn().mockReturnValue('blob:test');
    const revokeObjectURL = vi.fn();
    window.URL.createObjectURL = createObjectURL;
    window.URL.revokeObjectURL = revokeObjectURL;

    renderTab();

    await waitFor(() => {
      expect(screen.getByText('2026-11-14')).toBeInTheDocument();
    });

    const downloadBtn = screen.getByRole('button', { name: /Download archive for 2026-11-14/i });
    await userEvent.click(downloadBtn);

    await waitFor(() => {
      expect(downloadedDate).toBe('2026-11-14');
      expect(createObjectURL).toHaveBeenCalled();
    });
  });
});
