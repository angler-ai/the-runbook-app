// The flagship `post-deploy` walked through in the browser (M6): Alice
// starts it from the start dialog on a phone, an agent session does the
// agent steps through the CLI with a run token, Alice completes her
// checklist, Carol is refused, Bob decides the approval in a second browser
// context while Alice's page updates live, the run completes, and the
// Markdown export matches `runbook run report --format md`. Runs the suite
// ends early are cancelled through the UI by Alice, a project operator, with
// her browser token that carries executions:admin (D27: a run role plus that
// scope; `<controller>.control`, seeded by the harness).
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import type { Browser, Page } from '@playwright/test';
import {
  AgentDriver,
  apiGet,
  asPerson,
  cliMarkdownReport,
  sectionStepRun,
  startRun,
  waitFor,
} from './support/cli.ts';
import {
  controlTokenFile,
  expect,
  openAs,
  PHONE,
  recordBrowserIssues,
  signIn,
  signInWithFile,
  test,
  type PersonPage,
} from './support/fixtures.ts';
import type { E2EState } from './support/state.ts';

const NOT_ELIGIBLE = 'You are not eligible to decide this approval.';

/** My tasks, freshly loaded: the Approvals (n) and Tasks (n) group counts. */
async function inboxCounts(page: Page): Promise<{ approvals: number; tasks: number }> {
  await page.goto('/tasks');
  await expect(page.getByRole('heading', { name: 'My tasks' })).toBeVisible();
  const list = page.locator('[data-slot="inbox-list"], [data-slot="empty-state"]');
  await expect(list.or(page.getByText('Inbox is clear')).first()).toBeVisible();
  const count = async (label: string) => {
    const heading = page.getByRole('heading', { name: new RegExp(`^${label} \\(\\d+\\)$`) });
    if ((await heading.count()) === 0) return 0;
    return Number(/\((\d+)\)/.exec(await heading.innerText())?.[1] ?? 0);
  };
  return { approvals: await count('Approvals'), tasks: await count('Tasks') };
}

/** Starts post-deploy through ⌘K → Start a run, as the dialog would be used on a phone. */
async function startFromDialog(
  page: Page,
  inputs: { service: string; version: string },
): Promise<string> {
  await page.getByRole('button', { name: 'Open command palette' }).click();
  const palette = page.getByRole('dialog');
  await palette.getByRole('option', { name: 'Start a run' }).click();

  const dialog = page.getByRole('dialog', { name: 'Start a run' });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByLabel('Runbook')).toHaveValue(/.+/);
  await expect(dialog.getByLabel('Runbook').locator('option:checked')).toHaveText(
    'Post-deploy verification',
  );

  const inputsSet = dialog.getByRole('group', { name: 'Inputs' });
  await inputsSet.getByRole('combobox', { name: /service/ }).click();
  await page.getByRole('option', { name: inputs.service, exact: true }).click();
  await inputsSet.getByRole('textbox', { name: /version/ }).fill(inputs.version);
  await inputsSet.getByRole('button', { name: 'Save inputs' }).click();
  await expect(dialog.getByText('Inputs saved.')).toBeVisible();

  const fieldsSet = dialog.getByRole('group', { name: 'Fields' });
  await fieldsSet.getByRole('button', { name: 'Save fields' }).click();
  await expect(dialog.getByText('Fields saved.')).toBeVisible();

  await dialog.getByRole('button', { name: 'Start run' }).click();
  await expect(page).toHaveURL(/\/x\/exe_[0-9A-Z]+$/);
  return /\/x\/(exe_[0-9A-Z]+)$/.exec(page.url())?.[1] as string;
}

/**
 * Ticks one checklist item and waits until the saved tick is back in the
 * refreshed document. (Ticking faster is covered by "checklist ticks in
 * quick succession are both kept", R53.)
 */
async function tick(page: Page, panel: ReturnType<Page['getByTestId']>, exe: string, item: string) {
  let saved = false;
  const refreshed = page.waitForResponse((r) => {
    const path = new URL(r.url()).pathname;
    if (r.request().method() === 'POST' && path.endsWith('/checklist')) {
      saved = r.ok();
      return false;
    }
    return saved && r.request().method() === 'GET' && path === `/v1/executions/${exe}/report`;
  });
  const box = panel.getByRole('checkbox', { name: item });
  await box.click();
  await refreshed;
  await expect(box).toBeChecked();
  await expect(box).toBeEnabled();
}

/** Alice signed in with her executions:admin browser token, on a run's page. */
async function openControlPage(browser: Browser, stack: E2EState, exe: string) {
  const context = await browser.newContext({ baseURL: stack.baseURL });
  recordBrowserIssues(context);
  const page = await context.newPage();
  await signInWithFile(page, controlTokenFile(stack));
  await expect(page).toHaveURL(/\/tasks$/);
  await page.goto(`/x/${exe}`);
  return { context, page };
}

/** Cancel a run from its page (the signed-in token carries executions:admin). */
async function cancelInUi(page: Page) {
  await page.getByRole('button', { name: 'Cancel run' }).click();
  const confirm = page.getByRole('alertdialog', { name: 'Cancel this run?' });
  await confirm.getByRole('button', { name: 'Cancel run' }).click();
  await expect(confirm).toBeHidden();
  await expect(page.getByRole('alert')).toHaveCount(0);
}

/**
 * Ends a run so the next run on its service is not queued behind it
 * (post-deploy admits one run per service): Alice cancels it in the browser
 * with her token that carries executions:admin (D27), an agent session does the
 * `finally` step (notify), and this waits until the run is terminal.
 */
async function finishRun(browser: Browser, stack: E2EState, exe: string): Promise<void> {
  const { context, page } = await openControlPage(browser, stack, exe);
  try {
    await cancelInUi(page);
    const agent = await AgentDriver.start(stack, exe);
    await agent.doStep('notify', { summary: 'Cancelled by the e2e suite' });
    const owner = join(stack.pilotDir, 'owner.pat');
    await waitFor(async () => {
      const res = await apiGet(stack, owner, `/v1/executions/${exe}`);
      const status = ((await res.json()) as { status?: string }).status ?? '';
      return ['succeeded', 'failed', 'cancelled'].includes(status);
    }, `${exe} to finish`);
  } finally {
    await context.close();
  }
}

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

test.describe('flagship walkthrough', () => {
  let alice: PersonPage;
  let bob: PersonPage;
  let carol: PersonPage;

  test.afterAll(async () => {
    for (const p of [alice, bob, carol]) await p?.context.close();
  });

  test('start, agent and human steps, live approval, completion and export', async ({
    browser,
    stack,
  }) => {
    test.setTimeout(180_000);
    alice = await openAs(browser, stack, 'alice', { viewport: PHONE, hasTouch: true });
    bob = await openAs(browser, stack, 'bob');
    carol = await openAs(browser, stack, 'carol');
    const a = alice.page;

    let exe = '';
    // Other specs may leave work in these inboxes: counts are checked as
    // changes from this baseline.
    type Counts = { approvals: number; tasks: number };
    const base: Record<'alice' | 'bob' | 'carol', Counts> = {
      alice: { approvals: 0, tasks: 0 },
      bob: { approvals: 0, tasks: 0 },
      carol: { approvals: 0, tasks: 0 },
    };
    const plus = (who: keyof typeof base, approvals: number, tasks: number): Counts => ({
      approvals: base[who].approvals + approvals,
      tasks: base[who].tasks + tasks,
    });
    await test.step('My tasks before the run', async () => {
      base.alice = await inboxCounts(a);
      base.bob = await inboxCounts(bob.page);
      base.carol = await inboxCounts(carol.page);
    });

    await test.step('Alice starts the flagship from the start dialog (390×844)', async () => {
      exe = await startFromDialog(a, { service: 'api', version: 'v2.0.0' });
      await expect(a.getByRole('heading', { name: 'Verify api v2.0.0' })).toBeVisible();
      // U4: her narrow token cannot pause or cancel, and the page says why.
      await expect(a.getByText(/need a token with the executions:admin scope/)).toBeVisible();
      await expect(a.getByRole('button', { name: 'Cancel run' })).toHaveCount(0);
    });

    const agent =
      await test.step('the launcher mints a run token; the agent reports api_health', async () => {
        const driver = await AgentDriver.start(stack, exe);
        await driver.smoke();
        return driver;
      });

    const login = await stepRunOf(stack, exe, 'login_flow');
    const health = await stepRunOf(stack, exe, 'api_health');
    await expect(section(a, health).getByText('Succeeded')).toBeVisible();

    await test.step('Alice claims login_flow; focus stays with the step; My tasks counts it', async () => {
      await actions(a, login).getByRole('button', { name: 'Claim this step' }).click();
      await expect(actions(a, login).getByRole('group', { name: 'Checklist form' })).toBeVisible();
      // U11: the Claim button is gone; focus is on the step's heading, not <body>.
      await expect(section(a, login).getByRole('heading', { level: 2 })).toBeFocused();
      await expect(a).toHaveTitle(/^Verify api v2\.0\.0 \((running|waiting)\) · The Runbook$/);
      expect(await inboxCounts(a)).toEqual(plus('alice', 0, 1));
      expect(await inboxCounts(bob.page)).toEqual(plus('bob', 0, 0));
      // Back to the run from the inbox row.
      await a.getByRole('button', { name: new RegExp(`Log in as the test user.*${exe}`) }).click();
      await expect(a).toHaveURL(new RegExp(`/x/${exe}$`));
    });

    await test.step('Alice ticks her checklist and reports pass', async () => {
      const panel = actions(a, login);
      for (const item of ['Login page loads', 'Dashboard shows data']) {
        await tick(a, panel, exe, item);
      }
      await expect(panel.getByRole('checkbox', { name: 'Login page loads' })).toBeChecked();
      await panel.getByLabel('Check verdict, if this step is a check').selectOption('pass');
      await panel.getByRole('button', { name: 'Done' }).click();
      await expect(section(a, login).getByText('Succeeded')).toBeVisible();
      expect(await inboxCounts(a)).toEqual(plus('alice', 0, 0));
      await a.goto(`/x/${exe}`);
    });

    const signoff =
      await test.step('the agent reports the regions (eu-west fails) and the re-check', async () => {
        await agent.regionsUnhealthy();
        return stepRunOf(stack, exe, 'signoff');
      });

    await test.step('the approval opens on Alice’s page without a reload', async () => {
      await expect(actions(a, signoff).getByText('Approval required')).toBeVisible();
      await expect(
        actions(a, signoff).getByText('Checks failed for v2.0.0. Choose an action.'),
      ).toBeVisible();
      expect(await inboxCounts(a)).toEqual(plus('alice', 1, 0));
      expect(await inboxCounts(bob.page)).toEqual(plus('bob', 1, 0));
      expect(await inboxCounts(carol.page)).toEqual(plus('carol', 0, 0));
    });

    await test.step('Carol (eng-leads, escalation only) is refused before escalation', async () => {
      const c = carol.page;
      await c.goto(`/x/${exe}`);
      const panel = actions(c, signoff);
      await expect(panel.getByText('Approval required')).toBeVisible();
      await expect(panel.getByText(NOT_ELIGIBLE)).toBeVisible();
      await expect(panel.getByRole('button', { name: 'keep' })).toBeDisabled();
      await expect(panel.getByRole('button', { name: 'rollback' })).toBeDisabled();
      // The server is the authority: the same decision through the API is refused.
      const refused = await asPerson(stack, 'carol', [
        'step',
        'decide',
        signoff,
        '--option',
        'keep',
      ]);
      expect(refused.code).not.toBe(0);
      expect(refused.stderr).toContain('approval.not_eligible');
    });

    await test.step('Bob decides keep in his own context; Alice’s page follows live', async () => {
      // Alice's tab must not reload from here on: a marker on window would vanish.
      await a.goto(`/x/${exe}`);
      await expect(actions(a, signoff).getByText('Approval required')).toBeVisible();
      await a.evaluate(() => {
        (window as unknown as { __e2eNoReload: boolean }).__e2eNoReload = true;
      });
      await expect(a.getByText('Live', { exact: true })).toBeVisible();
      // From here Alice's page must not need a new stream (a reconnect or a
      // fall back to polling would open one).
      let streamOpens = 0;
      a.on('request', (request) => {
        if (new URL(request.url()).pathname === `/v1/executions/${exe}/events`) streamOpens += 1;
      });

      const b = bob.page;
      await b.getByRole('button', { name: new RegExp(`Roll back or keep\\?.*${exe}`) }).click();
      await expect(b).toHaveURL(new RegExp(`/x/${exe}$`));
      const panel = actions(b, signoff);
      await panel
        .getByLabel('Decision comment (optional)')
        .fill('Recovered after the re-check; keep v2.0.0');
      await panel.getByRole('button', { name: 'keep' }).click();
      // Every vote is confirmed (U16): a recorded vote cannot be changed.
      const confirm = b.getByRole('alertdialog', { name: 'Confirm: keep' });
      await confirm.getByRole('button', { name: 'keep' }).click();
      await expect(panel.getByText('chose keep')).toBeVisible();

      // U31: live, not polled. The page polls only after the stream fails,
      // and then every 10 s; the stream brings the decision in well under 3 s.
      const LIVE = { timeout: 3_000 };
      await expect(actions(a, signoff).getByText('Recorded Decisions (1)')).toBeVisible(LIVE);
      await expect(actions(a, signoff).getByText('chose keep')).toBeVisible(LIVE);
      await expect(section(a, signoff).getByText('Succeeded')).toBeVisible(LIVE);
      await expect(a.getByRole('status').filter({ hasText: /^Live$/ })).toBeVisible();
      expect(streamOpens, 'Alice’s page reopened its stream').toBe(0);
      // U11: the change is read out politely on Alice's page.
      const updates = a.getByRole('log', { name: 'Run updates' });
      await expect(updates).toHaveAttribute('aria-live', 'polite');
      await expect(updates).toContainText(/Step \d+, [^:]+: succeeded/);
      expect(
        await a.evaluate(() => (window as unknown as { __e2eNoReload?: boolean }).__e2eNoReload),
      ).toBe(true);
    });

    await test.step('the agent posts the report (finally) and the run completes live', async () => {
      await agent.doStep('notify', { summary: 'Report posted to #deploys' });
      await expect(a.getByRole('status').filter({ hasText: 'Completed' })).toBeVisible();
      await expect(
        a.locator('header, [data-slot="document-header"]').getByText('Succeeded').first(),
      ).toBeVisible();
      expect(
        await a.evaluate(() => (window as unknown as { __e2eNoReload?: boolean }).__e2eNoReload),
      ).toBe(true);
    });

    await test.step('the downloaded Markdown equals `runbook run report --format md`', async () => {
      const download = a.waitForEvent('download');
      await a.getByRole('button', { name: 'Download Markdown' }).click();
      const file = await (await download).path();
      const web = await readFile(file, 'utf8');
      const cli = await cliMarkdownReport(stack, 'alice', exe);
      expect(web.length).toBeGreaterThan(0);
      expect(web.startsWith('# Verify api v2\\.0\\.0\n')).toBe(true);
      expect(web).toContain('chose keep');
      expect(web).toBe(cli);
    });

    await test.step('My tasks are empty again for everyone', async () => {
      expect(await inboxCounts(a)).toEqual(plus('alice', 0, 0));
      expect(await inboxCounts(bob.page)).toEqual(plus('bob', 0, 0));
    });
  });

  // R53 (found by M6): a tick made right after the previous one settled was
  // sent with the previous tick cleared ([false, true]): the optimistic
  // overlay was dropped before the document refetch landed, so the form
  // re-derived the list from the stale document, and Done was then refused
  // with step.checklist_incomplete. Ticks now stay local while the claim is
  // held and are saved one request at a time, always the latest list.
  test('checklist ticks in quick succession are both kept', async ({ browser, stack }) => {
    // On `api`, which the walkthrough above leaves finished; ended at the
    // end so --repeat-each and later specs never queue behind it.
    const exe = await startRun(stack, 'alice', { service: 'api', version: 'v2.0.1' });
    const alice = await openAs(browser, stack, 'alice', { viewport: PHONE, hasTouch: true });
    // A slow network, made deterministic: document refetches are held while
    // the two ticks are made, so the page has only the stale (unticked)
    // server copy when the second tick goes out.
    let holding = false;
    let release: () => void = () => {};
    const held = new Promise<void>((r) => (release = r));
    try {
      const a = alice.page;
      await a.route(`**/v1/executions/${exe}/report*`, async (route) => {
        if (holding) await held;
        await route.continue();
      });
      await a.goto(`/x/${exe}`);
      const login = await stepRunOf(stack, exe, 'login_flow');
      const panel = actions(a, login);
      await panel.getByRole('button', { name: 'Claim this step' }).click();
      await expect(panel.getByRole('group', { name: 'Checklist form' })).toBeVisible();
      holding = true;
      for (const item of ['Login page loads', 'Dashboard shows data']) {
        // As fast as a person can: the next click as soon as the form is enabled.
        const box = panel.getByRole('checkbox', { name: item });
        await box.click();
        await expect(box).toBeEnabled();
      }
      for (const item of ['Login page loads', 'Dashboard shows data']) {
        await expect(panel.getByRole('checkbox', { name: item })).toBeChecked();
      }
      release();
      await panel.getByLabel('Check verdict, if this step is a check').selectOption('pass');
      await panel.getByRole('button', { name: 'Done' }).click();
      await expect(section(a, login).getByText('Succeeded')).toBeVisible();
      await expect(panel.getByRole('alert')).toHaveCount(0);
    } finally {
      release();
      await alice.context.close();
      await finishRun(browser, stack, exe);
    }
  });

  // R54 (found by M6): the start dialog opened before the runbook catalog
  // loaded kept no runbook: StartRunForm seeded its selection once, as '',
  // and never updated, so the select showed the first runbook as chosen but
  // no version form loaded and "Start run" stayed disabled. The selection is
  // now derived from the props until the person picks one, and the form
  // query is keyed on the resolved runbook and version.
  test('start dialog opened before the catalog loads still offers the form', async ({
    page,
    stack,
  }) => {
    await signIn(page, stack, 'alice');
    let release: () => void = () => {};
    const held = new Promise<void>((r) => (release = r));
    await page.route(`**/v1/projects/${stack.projectId}/runbooks*`, async (route) => {
      await held;
      await route.continue();
    });
    await page.goto('/p/pilot/runbooks');
    await page.getByRole('button', { name: 'Start a run' }).click();
    release();
    const dialog = page.getByRole('dialog', { name: 'Start a run' });
    await expect(dialog.getByLabel('Runbook').locator('option')).toHaveText([
      'Post-deploy verification',
    ]);
    await expect(dialog.getByRole('button', { name: 'Save inputs' })).toBeVisible();
  });

  // R55 (found by M6): the document offered "Claim this step" to a person on
  // agent-only steps (here api_health, `actor: agent` by default), and the
  // server refused with 422 claim.actor_mismatch. Claim is now offered only
  // on ready tasks the human resume brief lists as a person's work (actor
  // human or any, or handed off to a person), never on approvals.
  test('agent-only steps do not offer Claim to a person', async ({ browser, page, stack }) => {
    // On `api` (the a11y spec leaves a `web` run open); ended at the end.
    const exe = await startRun(stack, 'alice', { service: 'api', version: 'v4.0.0' });
    try {
      const health = await stepRunOf(stack, exe, 'api_health');
      const login = await stepRunOf(stack, exe, 'login_flow');
      await signIn(page, stack, 'alice');
      await page.goto(`/x/${exe}`);
      await expect(section(page, health)).toBeVisible();
      // The human step offers Claim, so the claimable work has loaded...
      await expect(
        actions(page, login).getByRole('button', { name: 'Claim this step' }),
      ).toBeVisible();
      // ...and the agent-only step beside it does not.
      await expect(
        actions(page, health).getByRole('button', { name: 'Claim this step' }),
      ).toHaveCount(0);
    } finally {
      await finishRun(browser, stack, exe);
    }
  });
});
