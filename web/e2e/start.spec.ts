// The start dialog and the inbox against the real server (U9, U10, U19):
//   - an input edited after "Save inputs" is the one the run starts with;
//   - a 422 the browser cannot foresee (a list of the wrong item type)
//     lands on its field, and the corrected retry reuses the action's key;
//   - My tasks follows the inbox cursor to the last page and the sidebar
//     counts every task.
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { Page, Request } from '@playwright/test';
import { apiGet, asPersonOk, startRun, waitFor } from './support/cli.ts';
import { browserTokenFile, expect, navTaskCount, signIn, test } from './support/fixtures.ts';
import type { E2EState } from './support/state.ts';

const isStart = (request: Request) =>
  request.method() === 'POST' && new URL(request.url()).pathname.endsWith('/executions');

async function openStartDialog(page: Page, stack: E2EState) {
  await page.goto(`/p/${stack.project}/runbooks`);
  await page.getByRole('button', { name: 'Start a run' }).click();
  const dialog = page.getByRole('dialog', { name: 'Start a run' });
  await expect(dialog).toBeVisible();
  const runbook = dialog.getByLabel('Runbook');
  await expect(runbook.locator('option', { hasText: 'Post-deploy verification' })).toHaveCount(1);
  await runbook.selectOption({ label: 'Post-deploy verification' });
  const inputs = dialog.getByRole('group', { name: 'Inputs' });
  await expect(inputs.getByRole('combobox', { name: /service/ })).toBeVisible();
  return { dialog, inputs };
}

async function chooseService(page: Page, inputs: ReturnType<Page['getByRole']>, service: string) {
  await inputs.getByRole('combobox', { name: /service/ }).click();
  await page.getByRole('option', { name: service, exact: true }).click();
}

const executionId = (page: Page) => /\/x\/(exe_[0-9A-Z]+)$/.exec(page.url())?.[1] as string;

test.describe.serial('start dialog and inbox', () => {
  test('an input edited after saving is the one the run starts with (U9)', async ({
    page,
    stack,
  }) => {
    await signIn(page, stack, 'alice');
    const { dialog, inputs } = await openStartDialog(page, stack);
    await chooseService(page, inputs, 'api');
    await inputs.getByRole('textbox', { name: /version/ }).fill('v9.0.1');
    await inputs.getByRole('button', { name: 'Save inputs' }).click();
    await expect(dialog.getByText('Inputs saved.')).toBeVisible();

    // Change the service after saving: the saved notice goes away…
    await chooseService(page, inputs, 'worker');
    await expect(dialog.getByText('Inputs saved.')).toBeHidden();

    // …and Start sends what the form shows.
    const sent = page.waitForRequest(isStart);
    await dialog.getByRole('button', { name: 'Start run' }).click();
    const body = (await sent).postDataJSON() as { inputs: Record<string, unknown> };
    expect(body.inputs).toMatchObject({ service: 'worker', version: 'v9.0.1' });
    await expect(page).toHaveURL(/\/x\/exe_[0-9A-Z]+$/);

    const res = await apiGet(
      stack,
      browserTokenFile(stack, 'alice'),
      `/v1/executions/${executionId(page)}`,
    );
    expect(res.status).toBe(200);
    const run = (await res.json()) as { inputs: Record<string, unknown> };
    expect(run.inputs).toMatchObject({ service: 'worker', version: 'v9.0.1' });
  });

  test('a server 422 shows on its field; the corrected retry reuses the key (U10)', async ({
    page,
    stack,
  }) => {
    await signIn(page, stack, 'alice');
    const keys: (string | null)[] = [];
    page.on('request', (request) => {
      if (isStart(request)) keys.push(request.headers()['idempotency-key'] ?? null);
    });
    const { dialog, inputs } = await openStartDialog(page, stack);
    await chooseService(page, inputs, 'web');
    await inputs.getByRole('textbox', { name: /version/ }).fill('v9.0.2');
    // A JSON list the browser accepts, whose items the version says are strings.
    const regions = inputs.getByRole('textbox', { name: /regions/ });
    await regions.fill('[1, 2]');

    const rejected = page.waitForResponse((r) => isStart(r.request()));
    await dialog.getByRole('button', { name: 'Start run' }).click();
    const response = await rejected;
    expect(response.status()).toBe(422);
    const problem = (await response.json()) as {
      code: string;
      errors: { path: string; code: string }[];
    };
    expect(problem.code).toBe('execution.inputs_invalid');
    expect(problem.errors[0]).toMatchObject({ path: 'inputs.regions[0]', code: 'data.type_error' });

    await expect(regions).toHaveAttribute('aria-invalid', 'true');
    await expect(regions).toHaveAccessibleDescription(/wrong type/i);
    await expect(regions).toBeFocused();
    await expect(dialog.getByRole('alert')).toContainText('Check the marked fields.');
    await expect(page).toHaveURL(/\/runbooks$/);

    await regions.fill('["us-west", "eu-west"]');
    await expect(regions).not.toHaveAttribute('aria-invalid', 'true');
    await dialog.getByRole('button', { name: 'Start run' }).click();
    await expect(page).toHaveURL(/\/x\/exe_[0-9A-Z]+$/);
    expect(keys).toHaveLength(2);
    expect(keys[0]).toBeTruthy();
    expect(keys[1]).toBe(keys[0]);

    const res = await apiGet(
      stack,
      browserTokenFile(stack, 'alice'),
      `/v1/executions/${executionId(page)}`,
    );
    const run = (await res.json()) as { inputs: Record<string, unknown> };
    expect(run.inputs).toMatchObject({ service: 'web', regions: ['us-west', 'eu-west'] });
  });

  test('My tasks follows the inbox cursor to the last page (U19)', async ({ page, stack }) => {
    // A runbook whose only step is a task for Alice, started three times.
    const dir = mkdtempSync(join(tmpdir(), 'runbook-e2e-'));
    const file = join(dir, 'inbox-paging.yaml');
    writeFileSync(
      file,
      [
        'apiVersion: therunbook.app/v1',
        'kind: Runbook',
        'metadata: {id: inbox-paging, title: Inbox paging}',
        'spec:',
        '  inputs:',
        '    n: {type: integer, required: true}',
        '  steps:',
        `    - {id: chore, title: Paging chore, actor: human, assignee: 'user:${stack.users.alice}'}`,
        '',
      ].join('\n'),
    );
    await asPersonOk(
      stack,
      stack.editor,
      ['push', file, '--project', stack.project, '--draft'],
      'editor',
    );
    await asPersonOk(
      stack,
      stack.editor,
      ['publish', 'inbox-paging', '--project', stack.project],
      'editor',
    );
    const runs = new Set<string>();
    for (const n of ['1', '2', '3'])
      runs.add(await startRun(stack, 'alice', { n }, 'inbox-paging'));

    // Every open task Alice has, read straight from the server.
    const allTasks = async () => {
      const ids: string[] = [];
      let cursor: string | null = null;
      do {
        const query: string = cursor
          ? `?limit=100&cursor=${encodeURIComponent(cursor)}`
          : '?limit=100';
        const res = await apiGet(stack, browserTokenFile(stack, 'alice'), `/v1/me/tasks${query}`);
        const page = (await res.json()) as {
          items: { execution_id: string }[];
          next_cursor: string | null;
        };
        ids.push(...page.items.map((item) => item.execution_id));
        cursor = page.next_cursor;
      } while (cursor);
      return ids;
    };
    let total = 0;
    await waitFor(async () => {
      const ids = await allTasks();
      total = ids.length;
      return [...runs].every((exe) => ids.includes(exe));
    }, 'the three paging tasks in Alice’s inbox');

    // Small pages, so the browser has to follow the cursor (2..3 pages).
    const pageSize = Math.max(1, Math.ceil(total / 3));
    const cursors: (string | null)[] = [];
    await page.route('**/v1/me/tasks**', async (route) => {
      const url = new URL(route.request().url());
      cursors.push(url.searchParams.get('cursor'));
      url.searchParams.set('limit', String(pageSize));
      await route.continue({ url: url.toString() });
    });
    await signIn(page, stack, 'alice');
    await expect(page.locator('[data-slot="inbox-item-row"]')).toHaveCount(total);
    for (const exe of runs) {
      await expect(page.locator('[data-slot="inbox-item-row"]', { hasText: exe })).toHaveCount(1);
    }
    await expect.poll(() => navTaskCount(page)).toBe(total);
    expect(cursors.filter((cursor) => cursor !== null).length).toBeGreaterThanOrEqual(2);
  });
});
