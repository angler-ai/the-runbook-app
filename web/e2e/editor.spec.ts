// The runbook editor (M5) end to end, as the seeded editor (project role
// `editor`, a browser token with runbooks:write/publish and skills:read):
//   - open the flagship draft, break it, see the provisional diagnostic, fix
//     it, save (server diagnostics replace the browser's), publish a version;
//   - a concurrent CLI `runbook push --draft` makes the save a 412; keep mine
//     and retry, publish, and diff the two versions on the versions page;
//   - axe on the editor in light and dark.
// Edits only touch the flagship's description and comments, so later specs
// (flagship, a11y) keep running the same steps.
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { apiGet, asPersonOk } from './support/cli.ts';
import { axeViolations, expect, signInWithFile, test } from './support/fixtures.ts';
import { loadState, type E2EState } from './support/state.ts';

let runbookId = '';

// Monaco renders spaces as no-break spaces: match lines with \s.
const TIMEOUT_LINE = /^\s*timeout:\s2h$/;
const DESCRIPTION_LINE = /^\s*description:\sVerify\sa\sproduction\sdeploy/;

test.beforeAll(async () => {
  const stack = loadState();
  const res = await apiGet(
    stack,
    join(stack.tokensDir, `${stack.editor}.editor`),
    `/v1/projects/${stack.projectId}/runbooks`,
  );
  const catalog = (await res.json()) as { items: { runbook_id: string; slug: string }[] };
  runbookId = catalog.items.find((r) => r.slug === 'post-deploy')?.runbook_id ?? '';
  expect(runbookId).not.toBe('');
});

async function openEditor(page: Page, stack: E2EState) {
  await signInWithFile(page, join(stack.tokensDir, `${stack.editor}.editor`));
  await expect(page).toHaveURL(/\/tasks$/);
  await page.goto(`/p/pilot/runbooks/${runbookId}/editor`);
  await expect(page.getByRole('heading', { level: 1, name: 'Editor' })).toBeVisible();
  // Monaco is up (lazy chunk) and the browser compiler has run (the wasm worker).
  await expect(page.getByTestId('yaml-editor').locator('.monaco-editor')).toBeVisible();
  await expect(page.getByTestId('step-budget')).toHaveText(
    'Step budget: at most 20 step runs (provisional).',
  );
}

const diagnosticsHeading = (page: Page) =>
  page.getByTestId('diagnostics').getByRole('heading', { level: 2 });

const editorLine = (page: Page, text: RegExp) =>
  page.getByTestId('yaml-editor').locator('.view-line').filter({ hasText: text });

/** Put the caret at the end of the first rendered line matching `text`. */
async function endOfLine(page: Page, text: string | RegExp) {
  await page
    .getByTestId('yaml-editor')
    .locator('.view-line')
    .filter({ hasText: text })
    .first()
    .click();
  await page.keyboard.press('End');
}

async function latestVersion(stack: E2EState): Promise<number | null> {
  const res = await apiGet(
    stack,
    join(stack.tokensDir, `${stack.editor}.editor`),
    `/v1/runbooks/${runbookId}`,
  );
  return ((await res.json()) as { latest_version: number | null }).latest_version;
}

test.describe.serial('runbook editor', () => {
  test('edit the flagship draft: diagnostic, fix, save, publish', async ({ page, stack }) => {
    const before = (await latestVersion(stack)) ?? 0;
    await openEditor(page, stack);
    await expect(diagnosticsHeading(page)).toHaveText('Provisional diagnostics (0)');
    await expect(page.getByRole('button', { name: 'Save draft' })).toBeDisabled();

    // Break the execution timeout: the browser compiler flags it as you type.
    await endOfLine(page, TIMEOUT_LINE);
    await page.keyboard.type('x');
    await expect(diagnosticsHeading(page)).toHaveText('Provisional diagnostics (1)');
    const diagnostics = page.getByTestId('diagnostics');
    await expect(diagnostics).toContainText('spec.invalid_value');
    await expect(diagnostics).toContainText('line 12:14');
    await expect(page.getByTestId('yaml-editor').locator('.squiggly-error').first()).toBeVisible();
    await expect(page.getByTestId('step-budget')).toHaveText(
      'Step budget: unknown until the runbook compiles.',
    );
    await expect(page.getByText('Unsaved changes.')).toBeVisible();

    // Fix it, and make a real change to publish.
    await page.keyboard.press('Backspace');
    await expect(diagnosticsHeading(page)).toHaveText('Provisional diagnostics (0)');
    await endOfLine(page, DESCRIPTION_LINE);
    await page.keyboard.type(' Edited in the browser.');
    await expect(editorLine(page, /keep\sit\.\sEdited\sin\sthe\sbrowser\.$/)).toBeVisible();

    await page.getByRole('button', { name: 'Save draft' }).click();
    await expect(page.getByTestId('editor-notice')).toHaveText(
      /^Saved revision \d+\. The server found no problems\.$/,
    );
    await expect(diagnosticsHeading(page)).toHaveText('Server diagnostics (0)');

    await page.getByRole('button', { name: 'Publish' }).click();
    await expect(page.getByTestId('editor-notice')).toHaveText(`Published v${before + 1}.`);
    expect(await latestVersion(stack)).toBe(before + 1);

    // Publishing the same draft again changes nothing (D21).
    await page.getByRole('button', { name: 'Publish' }).click();
    await expect(page.getByTestId('editor-notice')).toHaveText(
      `Unchanged; same as v${before + 1}. Nothing new was published.`,
    );
  });

  test('a concurrent CLI push is a 412: keep mine, publish, diff', async ({ page, stack }) => {
    const before = (await latestVersion(stack)) ?? 0;
    await openEditor(page, stack);
    await endOfLine(page, DESCRIPTION_LINE);
    await page.keyboard.type(' Second browser edit.');
    await expect(editorLine(page, /browser\.\sSecond\sbrowser\sedit\.$/)).toBeVisible();

    // Meanwhile the CLI pushes another draft revision.
    const pulled = await asPersonOk(
      stack,
      stack.editor,
      ['pull', 'post-deploy', '--project', 'pilot'],
      'editor',
    );
    const dir = mkdtempSync(join(tmpdir(), 'runbook-e2e-'));
    const file = join(dir, 'post-deploy.yaml');
    writeFileSync(file, `${pulled.trimEnd()}\n# pushed from the CLI\n`);
    await asPersonOk(
      stack,
      stack.editor,
      ['push', file, '--project', 'pilot', '--draft'],
      'editor',
    );

    await page.getByRole('button', { name: 'Save draft' }).click();
    const conflict = page.getByTestId('conflict');
    await expect(conflict).toContainText('This draft changed elsewhere.');
    await expect(conflict).toContainText('nothing was overwritten');
    // Nothing was written: the server still has the CLI's text.
    const draft = await asPersonOk(
      stack,
      stack.editor,
      ['pull', 'post-deploy', '--project', 'pilot'],
      'editor',
    );
    expect(draft).toContain('# pushed from the CLI');
    expect(draft).not.toContain('Second browser edit.');

    await conflict.getByRole('button', { name: 'Keep mine and retry' }).click();
    await expect(page.getByTestId('editor-notice')).toHaveText(
      /^Saved revision \d+\. The server found no problems\.$/,
    );
    await expect(conflict).toBeHidden();
    await page.getByRole('button', { name: 'Publish' }).click();
    await expect(page.getByTestId('editor-notice')).toHaveText(`Published v${before + 1}.`);

    await page.goto(`/p/pilot/runbooks/${runbookId}/versions`);
    await expect(page.getByLabel('Compare from')).toHaveValue(String(before));
    await expect(page.getByLabel('Compare to')).toHaveValue(String(before + 1));
    await page.getByRole('button', { name: 'Compare versions' }).click();
    const diff = page.getByRole('region', { name: `Diff ${before} to ${before + 1}` });
    await expect(diff).toContainText(`Changes from ${before} to ${before + 1}`);
    await expect(diff).toContainText('Second browser edit.');
    expect(readFileSync(file, 'utf8')).toContain('# pushed from the CLI');
  });

  for (const colorScheme of ['light', 'dark'] as const) {
    test(`axe on the editor, ${colorScheme}`, async ({ page, stack }) => {
      await page.emulateMedia({ colorScheme });
      await openEditor(page, stack);
      await endOfLine(page, TIMEOUT_LINE);
      await page.keyboard.type('x');
      await expect(diagnosticsHeading(page)).toHaveText('Provisional diagnostics (1)');
      expect(await axeViolations(page)).toEqual([]);
      for (const tab of ['Graph', 'Outline'] as const) {
        await page.getByRole('tab', { name: tab }).click();
        await expect(page.getByRole('tabpanel', { name: tab })).toBeVisible();
        expect(await axeViolations(page)).toEqual([]);
      }
    });
  }
});

test.describe('outline accessibility', () => {
  // Fixed: src/ui/outline rendered its expand/collapse buttons at 16×16 px,
  // under the WCAG 2.2 24×24 px target size, with no visible focus outline.
  test('outline expanders are at least 24×24 px with a visible focus ring', async ({
    page,
    stack,
  }) => {
    await openEditor(page, stack);
    await page.getByRole('tab', { name: 'Outline' }).click();
    const expander = page.getByRole('button', { name: 'Collapse Smoke tests' });
    await expect(expander).toBeVisible();
    const box = await expander.boundingBox();
    expect(box?.width ?? 0).toBeGreaterThanOrEqual(24);
    expect(box?.height ?? 0).toBeGreaterThanOrEqual(24);
    // Keyboard focus: the row first, then Tab into its expander.
    await page.locator('[data-node-id="smoke"]').focus();
    await page.keyboard.press('Tab');
    await expect(expander).toBeFocused();
    const ring = await expander.evaluate((el) => {
      const style = getComputedStyle(el);
      return {
        visible: el.matches(':focus-visible'),
        style: style.outlineStyle,
        width: parseFloat(style.outlineWidth),
      };
    });
    expect(ring.visible).toBe(true);
    expect(ring.style).not.toBe('none');
    expect(ring.width).toBeGreaterThanOrEqual(2);
    expect(
      (await axeViolations(page, { include: '[data-slot="runbook-outline"]' })).filter(
        (v) => v.id === 'target-size',
      ),
    ).toEqual([]);
  });
});
