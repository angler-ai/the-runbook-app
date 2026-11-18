// The execution document's other actions against the real server (U31):
//   - the human report form: outputs and a summary in one report;
//   - signals: a wait step released from the page;
//   - pause, resume, cancel and rerun by Alice, a project operator, with her
//     browser token that carries executions:admin (`<controller>.control`);
//   - the live stream failing over and over: the page falls back to polling
//     and still follows the run.
// The flows run on `web-controls`, a runbook of their own (published here by
// the editor), so they never queue behind `post-deploy`. Every run started
// here is cancelled at the end.
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { apiGet, asPersonOk, sectionStepRun, startRun, waitFor } from './support/cli.ts';
import {
  browserTokenFile,
  controlTokenFile,
  expect,
  signIn,
  signInWithFile,
  test,
} from './support/fixtures.ts';
import { loadState, readSecret, type E2EState } from './support/state.ts';

const RUNBOOK = 'web-controls';
const SOURCE = [
  'apiVersion: therunbook.app/v1',
  'kind: Runbook',
  `metadata: {id: ${RUNBOOK}, title: Web controls}`,
  'spec:',
  '  execution:',
  '    title: "Controls ${{ inputs.tag }}"',
  '  inputs:',
  '    tag: {type: string, required: true}',
  '  steps:',
  '    - id: measure',
  '      title: Record the error rate',
  '      actor: human',
  '      outputs:',
  '        error_rate: {type: number, required: true}',
  '    - id: hold',
  '      title: Wait for the deploy',
  '      wait: {signal: deploy-done, timeout: 1h}',
  '    - id: confirm',
  '      title: Confirm the deploy',
  '      actor: human',
  '',
].join('\n');

const TERMINAL = ['succeeded', 'failed', 'cancelled'];

function section(page: Page, stepRunId: string) {
  return page.locator(`section[data-step-id="${stepRunId}"]`);
}

function actions(page: Page, stepRunId: string) {
  return page.getByTestId(`actions-${stepRunId}`);
}

async function stepRunOf(stack: E2EState, exe: string, path: string): Promise<string> {
  let sr: string | undefined;
  await waitFor(async () => {
    sr = await sectionStepRun(stack, 'alice', exe, path);
    return Boolean(sr);
  }, `section ${path}`);
  return sr as string;
}

/** The run as the server has it, read with Alice's browser token. */
async function runStatus(stack: E2EState, exe: string): Promise<string> {
  const res = await apiGet(stack, browserTokenFile(stack, 'alice'), `/v1/executions/${exe}`);
  return ((await res.json()) as { status?: string }).status ?? '';
}

/** Cancels a run straight through the API with Alice's control token, unless it has ended. */
async function cancelRun(stack: E2EState, exe: string) {
  if (TERMINAL.includes(await runStatus(stack, exe))) return;
  const res = await fetch(`${stack.apiURL}/v1/executions/${exe}`, {
    method: 'PATCH',
    headers: {
      Authorization: `Bearer ${readSecret(controlTokenFile(stack))}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({ action: 'cancel' }),
  });
  expect([200, 409], `cancel answered ${res.status}`).toContain(res.status);
  await waitFor(async () => TERMINAL.includes(await runStatus(stack, exe)), `${exe} to end`);
}

test.describe('run controls and step actions', () => {
  const started: string[] = [];

  test.beforeAll(async () => {
    const stack = loadState();
    const dir = mkdtempSync(join(tmpdir(), 'runbook-e2e-'));
    const file = join(dir, `${RUNBOOK}.yaml`);
    writeFileSync(file, SOURCE);
    await asPersonOk(
      stack,
      stack.editor,
      ['push', file, '--project', stack.project, '--draft'],
      'editor',
    );
    await asPersonOk(
      stack,
      stack.editor,
      ['publish', RUNBOOK, '--project', stack.project],
      'editor',
    );
  });

  test.afterAll(async () => {
    const stack = loadState();
    for (const exe of started) await cancelRun(stack, exe);
  });

  async function start(stack: E2EState, tag: string): Promise<string> {
    const exe = await startRun(stack, 'alice', { tag }, RUNBOOK);
    started.push(exe);
    return exe;
  }

  test('the human report form sends outputs and a summary; a signal releases the wait', async ({
    page,
    stack,
  }) => {
    const exe = await start(stack, 'report');
    const measure = await stepRunOf(stack, exe, 'measure');
    await signIn(page, stack, 'alice');
    await page.goto(`/x/${exe}`);

    await test.step('Alice claims the step and reports an output with a summary', async () => {
      const panel = actions(page, measure);
      await panel.getByRole('button', { name: 'Claim this step' }).click();
      await panel.getByRole('textbox', { name: /error_rate/ }).fill('0.004');
      await panel.getByRole('textbox', { name: /^Summary/ }).fill('Below the 1% budget');
      await panel.getByRole('button', { name: 'Done' }).click();
      await expect(section(page, measure).getByText('Succeeded')).toBeVisible();
      await expect(panel.getByRole('alert')).toHaveCount(0);
      // One report carried both, as the server recorded it.
      const res = await apiGet(
        stack,
        browserTokenFile(stack, 'alice'),
        `/v1/executions/${exe}/report`,
      );
      const report = (await res.json()) as {
        sections: { path: string; outputs: Record<string, unknown>; summary?: string | null }[];
      };
      const recorded = report.sections.find((s) => s.path === 'measure');
      expect(recorded?.outputs).toEqual({ error_rate: 0.004 });
      expect(recorded?.summary).toBe('Below the 1% budget');
    });

    await test.step('the wait step takes the deploy-done signal from the page', async () => {
      const hold = await stepRunOf(stack, exe, 'hold');
      const panel = actions(page, hold);
      await panel.getByLabel('Signal name').fill('deploy-done');
      await panel.getByRole('button', { name: 'Send signal' }).click();
      await expect(section(page, hold).getByText('Succeeded')).toBeVisible();
      await expect(panel.getByRole('button', { name: 'Send signal' })).toHaveCount(0);
      // The run moved on: the next human step is offered.
      const confirm = await stepRunOf(stack, exe, 'confirm');
      await expect(
        actions(page, confirm).getByRole('button', { name: 'Claim this step' }),
      ).toBeVisible();
    });
  });

  test('pause, resume, cancel and rerun as the operator with executions:admin', async ({
    page,
    stack,
  }) => {
    const exe = await start(stack, 'controls');
    await signInWithFile(page, controlTokenFile(stack));
    await expect(page).toHaveURL(/\/tasks$/);
    await page.goto(`/x/${exe}`);
    await expect(page.getByRole('heading', { name: 'Controls controls' })).toBeVisible();

    await test.step('Pause', async () => {
      await page.getByRole('button', { name: 'Pause', exact: true }).click();
      await expect(page.getByRole('button', { name: 'Resume', exact: true })).toBeVisible();
      await expect(page.getByRole('button', { name: 'Pause', exact: true })).toHaveCount(0);
      expect(await runStatus(stack, exe)).toBe('paused');
    });

    await test.step('Resume', async () => {
      await page.getByRole('button', { name: 'Resume', exact: true }).click();
      await expect(page.getByRole('button', { name: 'Pause', exact: true })).toBeVisible();
      expect(['running', 'waiting']).toContain(await runStatus(stack, exe));
    });

    await test.step('Cancel, after the confirmation', async () => {
      await page.getByRole('button', { name: 'Cancel run' }).click();
      const confirm = page.getByRole('alertdialog', { name: 'Cancel this run?' });
      await confirm.getByRole('button', { name: 'Cancel run' }).click();
      await expect(confirm).toBeHidden();
      await expect(page.getByRole('button', { name: 'Rerun' })).toBeVisible();
      await expect(page.getByRole('button', { name: 'Cancel run' })).toHaveCount(0);
      expect(await runStatus(stack, exe)).toBe('cancelled');
    });

    await test.step('Rerun opens a new run with the same inputs', async () => {
      await page.getByRole('button', { name: 'Rerun' }).click();
      await expect(page).not.toHaveURL(new RegExp(`/x/${exe}$`));
      await expect(page).toHaveURL(/\/x\/exe_[0-9A-Z]+$/);
      const rerun = /\/x\/(exe_[0-9A-Z]+)$/.exec(page.url())?.[1] as string;
      started.push(rerun);
      await expect(page.getByRole('heading', { name: 'Controls controls' })).toBeVisible();
      await expect(page.getByRole('button', { name: 'Pause', exact: true })).toBeVisible();
      const res = await apiGet(stack, browserTokenFile(stack, 'alice'), `/v1/executions/${rerun}`);
      const view = (await res.json()) as { inputs: Record<string, unknown> };
      expect(view.inputs).toMatchObject({ tag: 'controls' });
    });
  });

  test('a stream that keeps failing falls back to polling, and the page still follows the run', async ({
    page,
    stack,
  }) => {
    const exe = await start(stack, 'polling');
    const measure = await stepRunOf(stack, exe, 'measure');
    await signIn(page, stack, 'alice');
    // Every attempt to open the live stream fails, as a dropped connection does.
    let streamAttempts = 0;
    await page.route(`**/v1/executions/${exe}/events*`, async (route) => {
      streamAttempts += 1;
      await route.abort('failed');
    });
    await page.goto(`/x/${exe}`);
    await expect(
      actions(page, measure).getByRole('button', { name: 'Claim this step' }),
    ).toBeVisible();
    // Three failures in a row (with backoff) switch the page to polling.
    await expect(page.getByRole('status').filter({ hasText: 'Polling for updates' })).toBeVisible();
    expect(streamAttempts).toBeGreaterThanOrEqual(3);

    // Someone else finishes the step (here Alice from the CLI); the page has
    // no stream, so only a poll can bring it (every 10 s).
    await asPersonOk(stack, 'alice', ['step', 'claim', measure, '--session', `cli-${exe}`]);
    await asPersonOk(stack, 'alice', ['step', 'get', measure]);
    await asPersonOk(stack, 'alice', [
      'step',
      'report',
      measure,
      '--status',
      'succeeded',
      '--output',
      'error_rate=0.002',
      '--summary',
      'Reported from the CLI',
    ]);
    await expect(section(page, measure).getByText('Succeeded')).toBeVisible({ timeout: 25_000 });
    await expect(page.getByRole('status').filter({ hasText: 'Polling for updates' })).toBeVisible();
  });
});
