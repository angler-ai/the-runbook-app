import { useState } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Schemas } from '../../api/client';
import type { MeView } from '../../api/hooks';
import { tokenStore } from '../../api/token-store';
import { server } from '../../test/msw/server';
import { expectNoAxeViolations } from '../../test/axe';
import { StepActions } from './StepActions';
import { EvidenceList } from './evidence/EvidenceViewer';
import { EvidenceAttachment, type EvidenceUploadItem } from './evidence/EvidenceAttachment';
import { uploadEvidenceFile } from './evidence/upload';
import { tabClaims } from './session';

const USER_CONTENT_ORIGIN = 'https://usercontent.example.test';
const APP_ORIGIN = 'http://localhost:3000';

const me = {
  user: { id: 'usr_riley', name: 'Riley Chen', email: 'riley@example.test' },
  org: { id: 'org_1', title: 'Acme', role: 'admin' },
  credential: {
    kind: 'pat',
    scopes: ['executions:read', 'executions:act', 'executions:start'],
    actor_kind: 'human',
    expires_at_ms: null,
    projects: null,
  },
  teams: [],
} as unknown as MeView;

const metaWithUploads: Schemas['MetaView'] = {
  edition: 'self_hosted',
  version: '0.0.0',
  schema: {},
  capabilities: { uploads: true },
  limits: {
    max_request_bytes: 52428800,
    uploads: {
      client_resize_long_edge: 2048,
      image_bytes: 5 * 1024 * 1024, // 5 MB
      file_bytes: 10 * 1024 * 1024, // 10 MB
      image_max_dimension: 4096,
      image_max_pixels: 16777216,
      image_types: ['image/png', 'image/jpeg', 'image/webp', 'image/gif'],
      images_per_step_run: 10,
      files_per_execution: 50,
    },
  },
};

function queryWrapper() {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: 0 },
      mutations: { retry: false },
    },
  });
  return ({ children }: { children: React.ReactNode }) => (
    <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
  );
}

afterEach(() => {
  tokenStore.clear();
  sessionStorage.clear();
  tabClaims.clear('sr_1');
});

describe('Evidence Uploads and Thumbnails (UP5)', () => {
  it('requests slot, uploads directly with progress, finalizes, and reports evidence IDs', async () => {
    const user = userEvent.setup();
    const executionId = 'exe_123';
    const stepRunId = 'sr_1';

    let slotRequested = false;
    let putUploaded = false;
    let finalized = false;
    let reportedEvidence: string[] = [];

    server.use(
      http.get('*/v1/meta', () => HttpResponse.json(metaWithUploads)),
      http.get('*/v1/me', () => HttpResponse.json(me)),
      http.get('*/v1/me/tasks', () => HttpResponse.json({ items: [], next_cursor: null })),
      http.get('*/v1/projects/:p/members', () =>
        HttpResponse.json({ items: [{ user_id: 'usr_riley', role: 'operator' }], next_cursor: null }),
      ),
      http.get('*/v1/step-runs/:sr', () =>
        HttpResponse.json({
          step_run_id: stepRunId,
          instructions: 'Perform manual health check and attach screenshot.',
          checklist: [],
          output_schema: null,
          evidence: null,
        }),
      ),
      http.post('*/v1/step-runs/:sr/evidence', async ({ request }) => {
        slotRequested = true;
        const body = (await request.json()) as Schemas['CreateEvidenceRequest'];
        expect(body.name).toBe('screenshot.png');
        expect(body.type).toBe('image/png');
        return HttpResponse.json(
          {
            evidence_id: 'evd_01H1234567890ABCDEFGHJKMNP',
            upload_url: `${USER_CONTENT_ORIGIN}/u/org_1/prj_1/exe_123/evd_01H1234567890ABCDEFGHJKMNP?sig=abc`,
            headers: {
              'Content-Type': 'image/png',
              'Content-Length': String(body.size),
            },
            expires_at_ms: Date.now() + 900000,
          },
          { status: 201 },
        );
      }),
      http.options(/.*\/u\/.*/, () => {
        return new HttpResponse(null, {
          status: 204,
          headers: {
            'Access-Control-Allow-Origin': '*',
            'Access-Control-Allow-Methods': 'PUT, OPTIONS',
            'Access-Control-Allow-Headers': '*',
          },
        });
      }),
      http.put(/.*\/u\/.*/, () => {
        putUploaded = true;
        return new HttpResponse(null, {
          status: 200,
          headers: {
            'Access-Control-Allow-Origin': '*',
          },
        });
      }),
      http.post('*/v1/step-runs/:sr/evidence/:evd/finalize', () => {
        finalized = true;
        return HttpResponse.json({
          id: 'evd_01H1234567890ABCDEFGHJKMNP',
          step_run_id: stepRunId,
          name: 'screenshot.png',
          type: 'image/png',
          size: 1024,
          state: 'finalized',
          width: 800,
          height: 600,
          url: `${USER_CONTENT_ORIGIN}/u/org_1/prj_1/exe_123/evd_01H1234567890ABCDEFGHJKMNP`,
          thumbnail_url: `${USER_CONTENT_ORIGIN}/u/org_1/prj_1/exe_123/evd_01H1234567890ABCDEFGHJKMNP/thumbnail`,
          created_at_ms: Date.now(),
          finalized_at_ms: Date.now(),
        });
      }),
      http.post('*/v1/step-runs/:sr/report', async ({ request }) => {
        const body = (await request.json()) as Schemas['ReportRequest'];
        reportedEvidence = (body.evidence as string[]) ?? [];
        return HttpResponse.json({ ok: true });
      }),
    );

    // Set tab claim so the step is already claimed by this user
    tabClaims.set(stepRunId, { id: 'clm_1', generation: 1 }, Date.now());

    const section: Schemas['StepSection'] = {
      step_run_id: stepRunId,
      path: 'health_check',
      title: 'Health Check',
      kind: 'task',
      status: 'claimed',
      actor: { kind: 'human', user_id: 'usr_riley' },
      outputs: {},
      evidence: [],
      memory_read: [],
      amendments: [],
      amended: false,
      logs: { href: 'http://localhost:3000/v1/step-runs/sr_1/logs' },
      stale: false,
    };

    const step: Schemas['StepView'] = {
      id: stepRunId,
      path: 'health_check',
      status: 'claimed',
      actor: { kind: 'human', user_id: 'usr_riley' },
      claim: { id: 'clm_1', generation: 1 },
      checklist: [],
    } as unknown as Schemas['StepView'];

    const caps = { canAct: true, canControl: true, controlNeedsScope: false, canRerun: true };

    const { container } = render(
      <StepActions
        executionId={executionId}
        section={section}
        step={step}
        me={me}
        capabilities={caps}
        documentAt={Date.now()}
      />,
      { wrapper: queryWrapper() },
    );

    // Form is displayed
    await screen.findByText('Perform manual health check and attach screenshot.');
    expect(screen.getByText('Attach Evidence')).toBeInTheDocument();

    // 1. Select file for upload
    const file = new File(['mock-image-bytes'], 'screenshot.png', { type: 'image/png' });
    const fileInput = screen.getByTestId('evidence-file-input');
    await user.upload(fileInput, file);

    // 2. Upload lifecycle: slot -> PUT -> finalize -> ready
    await waitFor(() => {
      expect(slotRequested).toBe(true);
      expect(putUploaded).toBe(true);
      expect(finalized).toBe(true);
    });

    const readyBadge = await screen.findByText('Ready');
    expect(readyBadge).toBeInTheDocument();

    const img = screen.getByRole('img', { name: 'screenshot.png' });
    expect(img).toHaveAttribute(
      'src',
      `${USER_CONTENT_ORIGIN}/u/org_1/prj_1/exe_123/evd_01H1234567890ABCDEFGHJKMNP/thumbnail`,
    );

    // 3. Report step as done
    const doneButton = screen.getByRole('button', { name: 'Done' });
    await user.click(doneButton);

    await waitFor(() => {
      expect(reportedEvidence).toEqual(['evd_01H1234567890ABCDEFGHJKMNP']);
    });

    await expectNoAxeViolations(container);
  });

  it('renders upload progress state while uploading', () => {
    const items: EvidenceUploadItem[] = [
      {
        id: 'upload-progress-1',
        file: new File(['mock-bytes'], 'large-log.txt', { type: 'text/plain' }),
        mime: 'text/plain',
        status: 'uploading',
        progress: 45,
      },
    ];

    render(
      <EvidenceAttachment
        stepRunId="sr_1"
        uploadsEnabled={true}
        limits={metaWithUploads.limits.uploads}
        items={items}
        onItemsChange={vi.fn()}
      />,
    );

    expect(screen.getByText('Uploading… 45%')).toBeInTheDocument();
    const progressbar = screen.getByRole('progressbar', {
      name: 'Upload progress for large-log.txt',
    });
    expect(progressbar).toHaveAttribute('aria-valuenow', '45');
  });

  it('rejects oversized files and disallowed formats client-side before network request', async () => {
    let slotCalls = 0;
    server.use(
      http.get('*/v1/meta', () => HttpResponse.json(metaWithUploads)),
      http.post('*/v1/step-runs/:sr/evidence', () => {
        slotCalls++;
        return HttpResponse.json({});
      }),
    );

    let items: EvidenceUploadItem[] = [];
    const onItemsChange = vi.fn((newItems: EvidenceUploadItem[]) => {
      items = newItems;
    });

    const { rerender, container } = render(
      <EvidenceAttachment
        stepRunId="sr_1"
        uploadsEnabled={true}
        limits={metaWithUploads.limits.uploads}
        maxRequestBytes={metaWithUploads.limits.max_request_bytes}
        items={items}
        onItemsChange={onItemsChange}
      />,
    );

    // 1. Try uploading an SVG image (always disallowed)
    const svgFile = new File(['<svg></svg>'], 'vector.svg', { type: 'image/svg+xml' });
    const input = screen.getByTestId('evidence-file-input');
    fireEvent.change(input, { target: { files: [svgFile] } });

    expect(onItemsChange).toHaveBeenCalled();
    rerender(
      <EvidenceAttachment
        stepRunId="sr_1"
        uploadsEnabled={true}
        limits={metaWithUploads.limits.uploads}
        maxRequestBytes={metaWithUploads.limits.max_request_bytes}
        items={items}
        onItemsChange={onItemsChange}
      />,
    );

    expect(screen.getByText('SVG images are not supported as evidence.')).toBeInTheDocument();
    expect(slotCalls).toBe(0);

    // 2. Try uploading an oversized image (exceeds image_bytes limit of 5MB)
    const largeFile = new File([new Uint8Array(6 * 1024 * 1024)], 'huge.png', {
      type: 'image/png',
    });
    fireEvent.change(input, { target: { files: [largeFile] } });

    rerender(
      <EvidenceAttachment
        stepRunId="sr_1"
        uploadsEnabled={true}
        limits={metaWithUploads.limits.uploads}
        maxRequestBytes={metaWithUploads.limits.max_request_bytes}
        items={items}
        onItemsChange={onItemsChange}
      />,
    );

    expect(
      screen.getByText(/Image size \(6\.0 MB\) exceeds maximum allowed size of 5\.0 MB\./),
    ).toBeInTheDocument();
    expect(slotCalls).toBe(0);

    // 3. Try uploading HEIC image
    const heicFile = new File(['mock-heic'], 'photo.heic', { type: 'image/heic' });
    fireEvent.change(input, { target: { files: [heicFile] } });

    rerender(
      <EvidenceAttachment
        stepRunId="sr_1"
        uploadsEnabled={true}
        limits={metaWithUploads.limits.uploads}
        maxRequestBytes={metaWithUploads.limits.max_request_bytes}
        items={items}
        onItemsChange={onItemsChange}
      />,
    );

    expect(screen.getByText('HEIC images are not supported.')).toBeInTheDocument();
    expect(slotCalls).toBe(0);

    await expectNoAxeViolations(container);
  });

  it('handles a failed upload and supports retry', async () => {
    let attempt = 0;
    server.use(
      http.get('*/v1/meta', () => HttpResponse.json(metaWithUploads)),
      http.post('*/v1/step-runs/:sr/evidence', () =>
        HttpResponse.json(
          {
            evidence_id: 'evd_fail_retry',
            upload_url: `${USER_CONTENT_ORIGIN}/u/fail-retry`,
            headers: { 'Content-Type': 'application/pdf' },
            expires_at_ms: Date.now() + 900000,
          },
          { status: 201 },
        ),
      ),
      http.options(/.*\/u\/fail-retry/, () => {
        return new HttpResponse(null, {
          status: 204,
          headers: {
            'Access-Control-Allow-Origin': '*',
            'Access-Control-Allow-Methods': 'PUT, OPTIONS',
            'Access-Control-Allow-Headers': '*',
          },
        });
      }),
      http.put(/.*\/u\/fail-retry/, () => {
        attempt++;
        if (attempt === 1) {
          return new HttpResponse(null, {
            status: 500,
            headers: { 'Access-Control-Allow-Origin': '*' },
          });
        }
        return new HttpResponse(null, {
          status: 200,
          headers: { 'Access-Control-Allow-Origin': '*' },
        });
      }),
      http.post('*/v1/step-runs/:sr/evidence/:evd/finalize', () =>
        HttpResponse.json({
          id: 'evd_fail_retry',
          step_run_id: 'sr_1',
          name: 'report.pdf',
          type: 'application/pdf',
          size: 2048,
          state: 'finalized',
          url: `${USER_CONTENT_ORIGIN}/u/report.pdf`,
          thumbnail_url: null,
          created_at_ms: Date.now(),
        }),
      ),
    );

    function TestHost() {
      const [items, setItems] = useState<EvidenceUploadItem[]>([]);
      return (
        <EvidenceAttachment
          stepRunId="sr_1"
          uploadsEnabled={true}
          limits={metaWithUploads.limits.uploads}
          items={items}
          onItemsChange={setItems}
        />
      );
    }

    const { container } = render(<TestHost />);

    const pdfFile = new File(['pdf-data'], 'report.pdf', { type: 'application/pdf' });
    const input = screen.getByTestId('evidence-file-input');
    fireEvent.change(input, { target: { files: [pdfFile] } });

    // First attempt fails with 500
    const errorMsg = await screen.findByText(/Evidence upload failed with HTTP 500/);
    expect(errorMsg).toBeInTheDocument();

    // Retry button is available
    const retryBtn = screen.getByRole('button', { name: 'Retry upload for report.pdf' });
    fireEvent.click(retryBtn);

    // Second attempt succeeds
    const readyBadge = await screen.findByText('Ready');
    expect(readyBadge).toBeInTheDocument();
    expect(screen.getByText('Download file')).toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('allows removing an attached file before submit', async () => {
    let items: EvidenceUploadItem[] = [
      {
        id: 'upload-to-remove',
        file: new File(['data'], 'notes.txt', { type: 'text/plain' }),
        mime: 'text/plain',
        status: 'ready',
        progress: 100,
        evidence: {
          id: 'evd_notes',
          step_run_id: 'sr_1',
          name: 'notes.txt',
          type: 'text/plain',
          size: 4,
          state: 'finalized',
          url: `${USER_CONTENT_ORIGIN}/u/notes.txt`,
          thumbnail_url: null,
          created_at_ms: Date.now(),
        },
      },
    ];

    const onItemsChange = vi.fn((newItems: EvidenceUploadItem[]) => {
      items = newItems;
    });

    const { rerender } = render(
      <EvidenceAttachment
        stepRunId="sr_1"
        uploadsEnabled={true}
        limits={metaWithUploads.limits.uploads}
        items={items}
        onItemsChange={onItemsChange}
      />,
    );

    expect(screen.getByText('notes.txt')).toBeInTheDocument();

    const removeBtn = screen.getByRole('button', { name: 'Remove notes.txt' });
    fireEvent.click(removeBtn);

    expect(onItemsChange).toHaveBeenCalledWith([]);

    rerender(
      <EvidenceAttachment
        stepRunId="sr_1"
        uploadsEnabled={true}
        limits={metaWithUploads.limits.uploads}
        items={items}
        onItemsChange={onItemsChange}
      />,
    );

    expect(screen.queryByText('notes.txt')).not.toBeInTheDocument();
  });

  it('hides attach control and shows one-line explanation when uploads are disabled', async () => {
    const { container } = render(
      <EvidenceAttachment
        stepRunId="sr_1"
        uploadsEnabled={false}
        items={[]}
        onItemsChange={vi.fn()}
      />,
    );

    expect(screen.getByTestId('uploads-disabled-note')).toHaveTextContent(
      'Evidence uploads are disabled because no user-content origin is configured.',
    );
    expect(screen.queryByTestId('evidence-drop-zone')).not.toBeInTheDocument();
    expect(screen.queryByTestId('evidence-file-input')).not.toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('renders images as thumbnails from user-content origin and non-images with download link in EvidenceList', async () => {
    const finalizedEvidence: Schemas['EvidenceView'][] = [
      {
        id: 'evd_image_1',
        step_run_id: 'sr_1',
        name: 'architecture.png',
        type: 'image/png',
        size: 204800,
        state: 'finalized',
        width: 1280,
        height: 720,
        url: `${USER_CONTENT_ORIGIN}/u/architecture.png`,
        thumbnail_url: `${USER_CONTENT_ORIGIN}/u/architecture-thumb.png`,
        created_at_ms: Date.now(),
      },
      {
        id: 'evd_doc_1',
        step_run_id: 'sr_1',
        name: 'audit-log.csv',
        type: 'text/csv',
        size: 15360,
        state: 'finalized',
        url: `${USER_CONTENT_ORIGIN}/u/audit-log.csv`,
        thumbnail_url: null,
        created_at_ms: Date.now(),
      },
    ];

    server.use(
      http.get('*/v1/executions/:exe/evidence', () =>
        HttpResponse.json({ items: finalizedEvidence }),
      ),
    );

    const { container } = render(
      <EvidenceList
        evidence={['evd_image_1', 'evd_doc_1', 'https://example.com/external-ref']}
        executionId="exe_123"
      />,
      { wrapper: queryWrapper() },
    );

    // 1. Image thumbnail rendered from user-content origin
    const img = await screen.findByRole('img', { name: 'architecture.png' });
    expect(img).toHaveAttribute('src', `${USER_CONTENT_ORIGIN}/u/architecture-thumb.png`);
    expect(img.getAttribute('src')).not.toContain(APP_ORIGIN);

    // Clicking image opens full viewer dialog
    fireEvent.click(img);
    await screen.findByRole('dialog');
    expect(screen.getByText(/200\.0 KB · image\/png · 1280×720/)).toBeInTheDocument();
    const originalLink = screen.getByRole('link', { name: 'Open original in new tab' });
    expect(originalLink).toHaveAttribute('href', `${USER_CONTENT_ORIGIN}/u/architecture.png`);

    // Close dialog
    const closeBtn = screen.getByRole('button', { name: 'Close' });
    fireEvent.click(closeBtn);

    // 2. Non-image file rendered with name, size, type and download link
    expect(screen.getByText('audit-log.csv')).toBeInTheDocument();
    expect(screen.getByText(/\(15\.0 KB · text\/csv\)/)).toBeInTheDocument();
    const downloadLink = screen.getByRole('link', { name: 'Download audit-log.csv' });
    expect(downloadLink).toHaveAttribute('href', `${USER_CONTENT_ORIGIN}/u/audit-log.csv`);
    expect(downloadLink).toHaveAttribute('download', 'audit-log.csv');

    // 3. External HTTPS reference rendered with ExternalLink
    const externalLink = screen.getByRole('link', { name: 'https://example.com/external-ref' });
    expect(externalLink).toHaveAttribute('href', 'https://example.com/external-ref');

    await expectNoAxeViolations(container);
  });

  it('drives real upload progress events through XMLHttpRequest upload.onprogress', async () => {
    const progressUpdates: Array<{ loaded: number; total: number; percent: number }> = [];

    const originalSend = XMLHttpRequest.prototype.send;
    const sendSpy = vi.spyOn(XMLHttpRequest.prototype, 'send').mockImplementation(function (
      this: XMLHttpRequest,
      body,
    ) {
      if (this.upload) {
        this.upload.dispatchEvent(
          new ProgressEvent('progress', { lengthComputable: true, loaded: 500, total: 1000 }),
        );
        this.upload.dispatchEvent(
          new ProgressEvent('progress', { lengthComputable: true, loaded: 1000, total: 1000 }),
        );
      }
      return originalSend.call(this, body);
    });

    server.use(
      http.post('*/v1/step-runs/:sr/evidence', () =>
        HttpResponse.json(
          {
            evidence_id: 'evd_prog_1',
            upload_url: `${USER_CONTENT_ORIGIN}/u/progress-test`,
            headers: {},
            expires_at_ms: Date.now() + 900000,
          },
          { status: 201 },
        ),
      ),
      http.options(/.*\/u\/progress-test/, () =>
        new HttpResponse(null, {
          status: 204,
          headers: {
            'Access-Control-Allow-Origin': '*',
            'Access-Control-Allow-Methods': 'PUT, OPTIONS',
            'Access-Control-Allow-Headers': '*',
          },
        }),
      ),
      http.put(/.*\/u\/progress-test/, () =>
        new HttpResponse(null, {
          status: 200,
          headers: { 'Access-Control-Allow-Origin': '*' },
        }),
      ),
      http.post('*/v1/step-runs/:sr/evidence/:evd/finalize', () =>
        HttpResponse.json({
          id: 'evd_prog_1',
          step_run_id: 'sr_1',
          name: 'test.txt',
          type: 'text/plain',
          size: 1000,
          state: 'finalized',
          url: `${USER_CONTENT_ORIGIN}/u/test.txt`,
          thumbnail_url: null,
          created_at_ms: Date.now(),
        }),
      ),
    );

    const file = new File(['x'.repeat(1000)], 'test.txt', { type: 'text/plain' });
    await uploadEvidenceFile({
      stepRunId: 'sr_1',
      file,
      mime: 'text/plain',
      onProgress: (loaded, total, percent) => {
        progressUpdates.push({ loaded, total, percent });
      },
    });

    expect(progressUpdates).toContainEqual({ loaded: 500, total: 1000, percent: 50 });
    expect(progressUpdates).toContainEqual({ loaded: 1000, total: 1000, percent: 100 });

    sendSpy.mockRestore();
  });

  it('renders loaded and total bytes progress when present', () => {
    const items: EvidenceUploadItem[] = [
      {
        id: 'upload-progress-bytes',
        file: new File(['mock-bytes'], 'large-log.txt', { type: 'text/plain' }),
        mime: 'text/plain',
        status: 'uploading',
        progress: 45,
        loaded: 450 * 1024,
        total: 1000 * 1024,
      },
    ];

    render(
      <EvidenceAttachment
        stepRunId="sr_1"
        uploadsEnabled={true}
        limits={metaWithUploads.limits.uploads}
        items={items}
        onItemsChange={vi.fn()}
      />,
    );

    expect(screen.getByText(/Uploading… 450\.0 KB \/ 1000\.0 KB \(45%\)/)).toBeInTheDocument();
  });

  it('aborts in-flight XMLHttpRequest upload when user removes the item', async () => {
    let abortCalled = false;
    const originalAbort = XMLHttpRequest.prototype.abort;
    const abortSpy = vi.spyOn(XMLHttpRequest.prototype, 'abort').mockImplementation(function (
      this: XMLHttpRequest,
    ) {
      abortCalled = true;
      return originalAbort.call(this);
    });

    server.use(
      http.post('*/v1/step-runs/:sr/evidence', () =>
        HttpResponse.json(
          {
            evidence_id: 'evd_abort_1',
            upload_url: `${USER_CONTENT_ORIGIN}/u/abort-test`,
            headers: {},
            expires_at_ms: Date.now() + 900000,
          },
          { status: 201 },
        ),
      ),
      http.options(/.*\/u\/abort-test/, () =>
        new HttpResponse(null, {
          status: 204,
          headers: {
            'Access-Control-Allow-Origin': '*',
            'Access-Control-Allow-Methods': 'PUT, OPTIONS',
            'Access-Control-Allow-Headers': '*',
          },
        }),
      ),
      http.put(/.*\/u\/abort-test/, async () => {
        await new Promise((resolve) => setTimeout(resolve, 3000));
        return new HttpResponse(null, {
          status: 200,
          headers: { 'Access-Control-Allow-Origin': '*' },
        });
      }),
    );

    function Host() {
      const [items, setItems] = useState<EvidenceUploadItem[]>([]);
      return (
        <EvidenceAttachment
          stepRunId="sr_1"
          uploadsEnabled={true}
          limits={metaWithUploads.limits.uploads}
          items={items}
          onItemsChange={setItems}
        />
      );
    }

    let resolveSendCalled!: () => void;
    const sendCalledPromise = new Promise<void>((resolve) => {
      resolveSendCalled = resolve;
    });

    const origSend = XMLHttpRequest.prototype.send;
    const sendSpy = vi.spyOn(XMLHttpRequest.prototype, 'send').mockImplementation(function (
      this: XMLHttpRequest,
      body,
    ) {
      resolveSendCalled();
      return origSend.call(this, body);
    });

    render(<Host />);

    const file = new File(['upload data'], 'document-in-flight.pdf', { type: 'application/pdf' });
    const input = screen.getByTestId('evidence-file-input');
    fireEvent.change(input, { target: { files: [file] } });

    await screen.findByText('document-in-flight.pdf');
    await sendCalledPromise;

    const removeBtn = screen.getByRole('button', { name: 'Remove document-in-flight.pdf' });
    fireEvent.click(removeBtn);

    expect(abortSpy).toHaveBeenCalled();
    expect(abortCalled).toBe(true);

    await waitFor(() => {
      expect(screen.queryByText('document-in-flight.pdf')).not.toBeInTheDocument();
    });

    expect(screen.queryByRole('alert')).not.toBeInTheDocument();

    sendSpy.mockRestore();
    abortSpy.mockRestore();
  });

  it('allows non-hardcoded document types client-side and displays server refusal using friendlyError', async () => {
    server.use(
      http.post('*/v1/step-runs/:sr/evidence', () =>
        HttpResponse.json(
          {
            code: 'upload.type_unsupported',
            detail: 'Unsupported file type',
          },
          {
            status: 422,
            headers: { 'Content-Type': 'application/problem+json' },
          },
        ),
      ),
    );

    function Host() {
      const [items, setItems] = useState<EvidenceUploadItem[]>([]);
      return (
        <EvidenceAttachment
          stepRunId="sr_1"
          uploadsEnabled={true}
          limits={metaWithUploads.limits.uploads}
          items={items}
          onItemsChange={setItems}
        />
      );
    }

    render(<Host />);

    const customFile = new File(['tar-bytes'], 'backup.tar.gz', { type: 'application/gzip' });
    const input = screen.getByTestId('evidence-file-input');
    fireEvent.change(input, { target: { files: [customFile] } });

    expect(screen.queryByText(/File type.*is not supported as evidence/)).not.toBeInTheDocument();

    const errorMsg = await screen.findByText('This file format is not supported as evidence.');
    expect(errorMsg).toBeInTheDocument();
  });
});
