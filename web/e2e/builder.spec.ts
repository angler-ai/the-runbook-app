// UI-4: build a runbook end to end against a real server. Create it from
// the post-deploy template, add a parallel group and an approval in the
// graph builder, save (the first save creates the draft), publish, and
// start a run.
//
// In the harness: `npm run e2e -- e2e/builder.spec.ts` (the pilot project,
// as the editor). Against a review stack (scripts/dev/review.sh), whose
// owner's browser token can write and publish in project `ops`:
//   E2E_REVIEW_STATE_DIR=<its state dir> E2E_BASE_URL=http://127.0.0.1:<vite> \
//     npx playwright test e2e/builder.spec.ts
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { expect, test } from './support/fixtures.ts';
import { loadState, readSecret } from './support/state.ts';

function target(): { project: string; tokenFile: string } {
  const review = process.env.E2E_REVIEW_STATE_DIR;
  if (review) return { project: 'ops', tokenFile: join(review, 'browser.token') };
  const stack = loadState();
  return { project: stack.project, tokenFile: join(stack.tokensDir, `${stack.editor}.editor`) };
}

async function signIn(page: Page, tokenFile: string) {
  await page.goto('/sign-in');
  await page.getByLabel('Personal access token').fill(readSecret(tokenFile));
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page).not.toHaveURL(/\/sign-in/);
}

/** Open the palette from the toolbar's "Add step" (the end of the runbook) and pick a kind. */
async function addAtEnd(page: Page, kind: RegExp) {
  await page
    .getByRole('toolbar', { name: 'Builder' })
    .getByRole('button', { name: 'Add step' })
    .click();
  await page.getByRole('menuitem', { name: kind }).click();
}

test('create from the post-deploy template, add a parallel group and an approval, save, publish and run', async ({
  page,
}) => {
  const { project, tokenFile } = target();
  const stamp = Date.now().toString(36);
  const name = `Builder e2e ${stamp}`;
  const id = `builder-e2e-${stamp}`;
  await signIn(page, tokenFile);

  // The creation dialog: from a template, the name and the ID it gives.
  await page.goto(`/p/${project}/runbooks`);
  await page.getByRole('button', { name: 'New runbook' }).click();
  const dialog = page.getByRole('dialog', { name: 'New runbook' });
  // The choices are cards; their radios are for keyboards and screen readers.
  await dialog.getByText('From a template', { exact: true }).click();
  await expect(dialog.getByRole('radio', { name: /From a template/ })).toBeChecked();
  await expect(dialog.getByRole('radio', { name: /Post-deploy checks/ })).toBeChecked();
  await dialog.getByLabel('Name').fill(name);
  await expect(dialog.getByLabel('ID')).toHaveValue(id);
  await dialog.getByRole('button', { name: 'Continue to the builder' }).click();

  // The builder, unsaved.
  await expect(page).toHaveURL(/\/runbooks\/new\?/);
  await expect(page.getByRole('heading', { level: 1, name: 'New runbook' })).toBeVisible();
  await expect(page.locator('[data-node-id="health"]')).toBeVisible();
  await expect(page.getByText(/Not saved yet/)).toBeVisible();

  // A parallel group (two steps, any order) at the end of the run.
  await addAtEnd(page, /Parallel \(any order\)/);
  const parallel = page.locator('[data-slot="step-panel"]');
  await expect(parallel.getByRole('heading', { name: 'parallel', exact: true })).toBeFocused();
  await parallel.getByLabel('Title', { exact: true }).fill('Smoke tests');
  await expect(page.locator('[data-node-id="smoke_tests"]')).toBeVisible();

  // An approval after it, approved by the on-call team.
  await page.keyboard.press('Escape');
  await addAtEnd(page, /^Approval/);
  const approval = page.locator('[data-slot="step-panel"]');
  await expect(approval.getByRole('heading', { name: 'approval', exact: true })).toBeFocused();
  await approval.getByLabel('Title', { exact: true }).fill('Release manager signs off');
  await approval.getByLabel('Prompt', { exact: true }).fill('Ship it?');
  await approval.getByRole('button', { name: 'Add a team' }).click();
  await approval.getByRole('combobox', { name: 'Approving team 1' }).fill('oncall');
  await expect(page.getByTestId('diagnostics').getByRole('heading')).toHaveText(
    /Provisional diagnostics \(0\)/,
  );

  // The YAML tab holds the same draft: the new steps are at its end, the
  // approval's ID taken from its title.
  await page.getByRole('tab', { name: 'YAML' }).click();
  const yaml = page.getByTestId('yaml-editor');
  // Monaco draws only the lines in view: scroll down until the line shows.
  const approvalLine = yaml
    .locator('.view-line')
    .filter({ hasText: /id:\srelease_manager_signs_off/ });
  await yaml.locator('.view-lines').hover();
  for (let i = 0; i < 40 && (await approvalLine.count()) === 0; i += 1) {
    await page.mouse.wheel(0, 400);
    await page.waitForTimeout(50);
  }
  await expect(approvalLine).toHaveCount(1);
  await page.getByRole('tab', { name: 'Graph' }).click();

  // Save creates it as a draft; publishing is separate.
  await page.getByRole('button', { name: 'Save draft' }).click();
  await expect(page).toHaveURL(/\/runbooks\/rb_[0-9A-Z]+\/editor$/);
  await expect(page.getByTestId('editor-notice')).toContainText(
    'Created as a draft (revision 1). The server found no problems.',
  );
  await page.getByRole('button', { name: 'Publish' }).click();
  await expect(page.getByTestId('editor-notice')).toHaveText('Published v1.');

  // Start a run of it from its page.
  const runbookId = /\/runbooks\/(rb_[0-9A-Z]+)\/editor$/.exec(page.url())?.[1] ?? '';
  await page.goto(`/p/${project}/runbooks/${runbookId}`);
  await page.getByRole('button', { name: 'Start run' }).click();
  const start = page.getByRole('dialog', { name: 'Start a run' });
  const inputs = start.getByRole('group', { name: 'Inputs' });
  await inputs.getByRole('textbox', { name: /service/ }).fill('api');
  await inputs.getByRole('textbox', { name: /version/ }).fill('1.2.3');
  await start.getByRole('button', { name: 'Save inputs' }).click();
  await start.getByRole('button', { name: 'Start run' }).click();
  await expect(page).toHaveURL(/\/x\/exe_[0-9A-Z]+/);
  await expect(page.getByText('api 1.2.3: post-deploy checks').first()).toBeVisible();
});
