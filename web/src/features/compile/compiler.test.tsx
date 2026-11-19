// The editor over the BUILT browser compiler (src/wasm/runbook.wasm from
// `npm run wasm:build`; these tests skip without it, as src/wasm's do):
// editing the flagship updates the Outline view, and a stale directory gives
// provisional diagnostics that the server's save verdict replaces (F99).
import { configure, fireEvent, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { tokenStore } from '../../api/token-store';
import { server } from '../../test/msw/server';
import flagship from '../../wasm/fixtures/post-deploy.yaml.txt?raw';
import { loadCompiler, type RunbookCompiler } from '../../wasm/runbook';
import { RB, editorHandlers, etagOf, renderEditor } from '../editor/test-support';
import type { CompilerBackend } from './useCompiler';

vi.mock('../editor/YamlEditor', async () => ({
  default: (await import('../editor/test-support')).StubYamlEditor,
}));

// Detect the module on disk (see src/wasm/runbook.test.ts for why not a glob).
type FsPromises = {
  readFile: (path: string) => Promise<Uint8Array<ArrayBuffer>>;
  access: (path: string) => Promise<void>;
};
const fs = (await import(/* @vite-ignore */ 'node:fs/promises' as string)) as FsPromises;
const here = decodeURIComponent(new URL(import.meta.url).pathname);
const wasmPath = `${here.slice(0, here.lastIndexOf('/src/') + 5)}wasm/runbook.wasm`;
const isBuilt = await fs.access(wasmPath).then(
  () => true,
  () => false,
);
if (!isBuilt) console.warn('SKIPPED compiler editor tests: src/wasm/runbook.wasm is not built');

let compiler: RunbookCompiler;
const backend: CompilerBackend = {
  validate: async (request) => compiler.validate(request),
  outline: async (request) => compiler.outline(request),
  describe: async (request) => compiler.describe(request),
};

const TEAMS = [
  { id: 'team_01ARZ3NDEKTSV4RRFFQ69G5FAV', slug: 'oncall' },
  { id: 'team_01ARZ3NDEKTSV4RRFFQ69G5FAW', slug: 'eng-leads' },
];
const SKILLS = [
  { name: 'check-error-rates', hash: '01'.repeat(32), reference: './skills/check-error-rates' },
  { name: 'rollback-service', hash: '02'.repeat(32), reference: './skills/rollback-service' },
];

beforeEach(async () => {
  tokenStore.set('trb_pat_test');
  if (isBuilt) compiler = await loadCompiler(await fs.readFile(wasmPath));
});
afterEach(() => tokenStore.clear());

const diagnosticsHeading = () => within(screen.getByTestId('diagnostics')).getByRole('heading');

// The real compiler runs in the test: on a shared CI runner its first
// compile can take longer than findBy's default second.
configure({ asyncUtilTimeout: 10_000 });

describe.skipIf(!isBuilt)('editor over the built compiler', () => {
  it('editing the flagship updates the outline', async () => {
    server.use(
      ...editorHandlers({
        draft: { source: flagship, revision: 1 },
        latestVersion: 1,
        teams: TEAMS,
        skills: SKILLS,
      }),
    );
    renderEditor(backend);
    const yaml = await screen.findByRole('textbox', { name: 'Runbook YAML' });
    expect(
      await screen.findByText('Step budget: at most 20 step runs (provisional).'),
    ).toBeVisible();
    expect(diagnosticsHeading()).toHaveTextContent('Provisional diagnostics (0)');

    await userEvent.click(screen.getByRole('tab', { name: 'Outline' }));
    expect(await screen.findByText('Health endpoint returns 200')).toBeVisible();

    fireEvent.change(yaml, {
      target: {
        value: flagship.replace(
          'title: Health endpoint returns 200',
          'title: Health endpoint answers quickly',
        ),
      },
    });
    expect(await screen.findByText('Health endpoint answers quickly')).toBeVisible();
    expect(screen.queryByText('Health endpoint returns 200')).not.toBeInTheDocument();

    // Text that does not parse keeps the last outline, marked as such.
    fireEvent.change(yaml, { target: { value: `${flagship}\n  - [unclosed` } });
    expect(
      await screen.findByText(
        'The current text does not parse; this is the last outline that did.',
      ),
    ).toBeVisible();
    expect(screen.getByText('Health endpoint answers quickly')).toBeVisible();
  });

  it('a stale directory gives provisional diagnostics; the server’s save verdict wins (F99)', async () => {
    // The browser's directory predates the teams the flagship names.
    server.use(
      ...editorHandlers({
        draft: { source: flagship, revision: 1 },
        latestVersion: 1,
        teams: [],
        skills: SKILLS,
      }),
      http.patch('*/v1/runbooks/:rb/draft', async ({ request }) => {
        const body = (await request.json()) as { source: string };
        return HttpResponse.json(
          { runbook_id: RB, source: body.source, revision: 2, diagnostics: [] },
          { headers: { ETag: etagOf(2) } },
        );
      }),
    );
    renderEditor(backend);
    const yaml = await screen.findByRole('textbox', { name: 'Runbook YAML' });
    const markers = screen.getByRole('list', { name: 'Editor markers' });
    await within(markers).findAllByText(/^provisional /);
    const stale = within(markers).getAllByRole('listitem').length;
    expect(stale).toBeGreaterThan(0);
    expect(diagnosticsHeading()).toHaveTextContent(`Provisional diagnostics (${stale})`);

    fireEvent.change(yaml, { target: { value: `${flagship}# reviewed\n` } });
    await userEvent.click(screen.getByRole('button', { name: 'Save draft' }));
    expect(
      await screen.findByText(/Saved revision 2\. The server found no problems\./),
    ).toBeVisible();
    expect(diagnosticsHeading()).toHaveTextContent('Server diagnostics (0)');
    expect(within(markers).queryAllByRole('listitem')).toHaveLength(0);
  });
});
