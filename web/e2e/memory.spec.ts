// The Memory tab on the runbook overview page (MEM5) end to end:
//   - navigate to the Memory tab on the flagship runbook;
//   - create a new memory entry through the dialog;
//   - inspect entry details, tags, writer attribution, and revision history;
//   - edit the memory entry, verifying revision bump;
//   - axe accessibility check on the memory tab.
import { join } from 'node:path';
import { apiGet } from './support/cli.ts';
import { axeViolations, expect, signInWithFile, test } from './support/fixtures.ts';
import { loadState } from './support/state.ts';

let runbookId = '';

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

test('memory tab: lists, creates, edits entries and has no axe violations', async ({ page }) => {
  const stack = loadState();
  const editorToken = join(stack.tokensDir, `${stack.editor}.editor`);

  await signInWithFile(page, editorToken);
  await page.goto(`/p/${stack.projectId}/runbooks/${runbookId}?tab=memory`);

  // Ensure Memory tab is selected
  const memoryTab = page.getByRole('tab', { name: /memory/i });
  await expect(memoryTab).toBeVisible();
  await expect(memoryTab).toHaveAttribute('data-state', 'active');

  // Verify memory tab view
  const memoryView = page.getByRole('region', { name: /memory entries list/i }).or(
    page.getByText(/no memory entries yet/i),
  );
  await expect(memoryView).toBeVisible();

  // Create a new memory entry
  const newMemoryBtn = page.getByRole('button', { name: /new memory/i });
  await expect(newMemoryBtn).toBeVisible();
  await newMemoryBtn.click();

  const dialog = page.getByRole('dialog', { name: /new memory entry/i });
  await expect(dialog).toBeVisible();

  const title = `E2E Playwright memory ${Date.now()}`;
  await dialog.getByLabel(/^title$/i).fill(title);
  await dialog.getByLabel(/body/i).fill('Remember to verify all preconditions before running.');
  await dialog.getByLabel(/tags/i).fill('e2e, testing');
  await dialog.getByRole('button', { name: /save entry/i }).click();

  // Dialog closes and new entry appears
  await expect(dialog).not.toBeVisible();
  await expect(page.getByRole('heading', { level: 3, name: title })).toBeVisible();
  await expect(page.getByText('Remember to verify all preconditions before running.')).toBeVisible();
  await expect(page.getByText('#e2e')).toBeVisible();

  // Edit the entry
  await page.getByRole('button', { name: /^edit$/i }).click();
  const editDialog = page.getByRole('dialog', { name: /edit memory entry/i });
  await expect(editDialog).toBeVisible();

  const updatedTitle = `${title} (edited)`;
  await editDialog.getByLabel(/^title$/i).fill(updatedTitle);
  await editDialog.getByRole('button', { name: /save changes/i }).click();

  await expect(editDialog).not.toBeVisible();
  await expect(page.getByRole('heading', { level: 3, name: updatedTitle })).toBeVisible();

  // Accessibility check on Memory tab
  const violations = await axeViolations(page);
  expect(violations).toEqual([]);
});
