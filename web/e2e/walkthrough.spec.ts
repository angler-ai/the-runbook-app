// The flagship walkthrough with screenshots (plan §6, "Pilot done": Claude
// walks the flagship in a real browser as Alice, Bob and Carol, desktop and
// phone, and records screenshots under `.reviews/web-ui/`). Tagged
// @walkthrough and left out of `npm run e2e`; run it with
// `npm run e2e:walkthrough` (container mode: the packaged image, real CSP).
//
// It writes, to E2E_WALKTHROUGH_DIR (default `<checkout>/../.reviews/web-ui`,
// outside the repository):
//   - NN[x]-<desktop|phone>-<light|dark>-<what>.png, one per step and state;
//   - 12-export-verify-api-v2.0.0.md, the Markdown the page downloads;
//   - INDEX.md, one line per screenshot;
//   - walkthrough-log.txt: console errors and warnings, page errors, failed
//     requests and responses ≥ 400 (paths only), and phone layouts wider
//     than the viewport, each with the step it happened in.
// Screenshots are taken before any token is typed, and every password
// field is masked anyway, so no secret can reach an image.
import { execFile } from 'node:child_process';
import { copyFile, mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { Browser, BrowserContext, Locator, Page } from '@playwright/test';
import { AgentDriver, apiGet, cliMarkdownReport, sectionStepRun, waitFor } from './support/cli.ts';
import {
  browserTokenFile,
  expect,
  navTaskCount,
  PHONE,
  recordBrowserIssues,
  signInWithFile,
  test,
} from './support/fixtures.ts';
import type { E2EState } from './support/state.ts';

const DESKTOP = { width: 1440, height: 900 } as const;
const webDir = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const OUT = process.env.E2E_WALKTHROUGH_DIR ?? resolve(webDir, '..', '..', '.reviews', 'web-ui');
/** What this spec writes into OUT; only these are cleared before a run. */
const OWN_FILE = /^(\d\d[a-z]?-.+\.(png|md)|INDEX\.md|walkthrough-log\.txt)$/;

type Viewport = 'desktop' | 'phone';
type Scheme = 'light' | 'dark';

/** The step the walkthrough is on, for the log. */
let currentStep = '00 setup';
const log: string[] = [];
const index: { file: string; what: string }[] = [];

function note(line: string) {
  log.push(`[${currentStep}] ${line}`);
}

/** Paths only: never a query string, a header or a body. */
function pathOf(url: string): string {
  try {
    const parsed = new URL(url);
    return parsed.protocol === 'http:' || parsed.protocol === 'https:'
      ? parsed.pathname
      : `${parsed.protocol}…`;
  } catch {
    return url.slice(0, 80);
  }
}

function watch(context: BrowserContext, label: string) {
  recordBrowserIssues(context);
  context.on('console', (message) => {
    if (message.type() !== 'error' && message.type() !== 'warning') return;
    const text = message.text().replace(/data:[^'"\s)]{40,}/g, (uri) => `${uri.slice(0, 30)}…`);
    note(`${label}: console.${message.type()}: ${text.slice(0, 300)}`);
  });
  context.on('weberror', (error) => {
    note(`${label}: pageerror: ${error.error().message.slice(0, 300)}`);
  });
  context.on('requestfailed', (request) => {
    note(
      `${label}: requestfailed: ${request.method()} ${pathOf(request.url())} (${request.failure()?.errorText ?? ''})`,
    );
  });
  context.on('response', (response) => {
    if (response.status() < 400) return;
    note(
      `${label}: response ${response.status()}: ${response.request().method()} ${pathOf(response.url())}`,
    );
  });
}

interface Who {
  label: string;
  viewport: Viewport;
  context: BrowserContext;
  page: Page;
}

async function newContext(browser: Browser, stack: E2EState, label: string, viewport: Viewport) {
  const context = await browser.newContext({
    baseURL: stack.baseURL,
    viewport: viewport === 'phone' ? PHONE : DESKTOP,
    ...(viewport === 'phone' ? { hasTouch: true, deviceScaleFactor: 2 } : {}),
    colorScheme: 'light',
  });
  watch(context, label);
  return { label, viewport, context, page: await context.newPage() } satisfies Who;
}

/** A person signed in with a token file, on My tasks. */
async function openPerson(
  browser: Browser,
  stack: E2EState,
  label: string,
  viewport: Viewport,
  tokenFile: string,
): Promise<Who> {
  const who = await newContext(browser, stack, label, viewport);
  await signInWithFile(who.page, tokenFile);
  await expect(who.page).toHaveURL(/\/tasks$/);
  await expect(who.page.getByRole('heading', { name: 'My tasks' })).toBeVisible();
  return who;
}

/** Elements wider than a phone viewport, recorded as layout findings. */
async function checkOverflow(who: Who, file: string) {
  if (who.viewport !== 'phone') return;
  const found = await who.page.evaluate(() => {
    const width = document.documentElement.clientWidth;
    const scroll = document.documentElement.scrollWidth;
    const culprits: string[] = [];
    for (const el of Array.from(document.body.querySelectorAll('*'))) {
      const box = el.getBoundingClientRect();
      if (box.width === 0 || box.right <= width + 1) continue;
      // Report the outermost offender only: skip children of one already listed.
      let parent = el.parentElement;
      let nested = false;
      while (parent && parent !== document.body) {
        if (parent.getBoundingClientRect().right > width + 1) {
          const overflow = getComputedStyle(parent).overflowX;
          if (overflow === 'visible') {
            nested = true;
            break;
          }
        }
        parent = parent.parentElement;
      }
      if (nested) continue;
      // Inside a scroll container, wide content is intended (code, tables).
      let clipped = false;
      for (let p = el.parentElement; p && p !== document.body; p = p.parentElement) {
        const o = getComputedStyle(p).overflowX;
        if (o === 'auto' || o === 'scroll' || o === 'hidden' || o === 'clip') {
          clipped = true;
          break;
        }
      }
      if (clipped) continue;
      const id = el.getAttribute('data-testid') ?? el.getAttribute('data-slot') ?? '';
      const text = (el.textContent ?? '').trim().slice(0, 40);
      culprits.push(
        `<${el.tagName.toLowerCase()}${id ? ` ${id}` : ''}> right=${Math.round(box.right)} "${text}"`,
      );
      if (culprits.length >= 5) break;
    }
    // Containers that scroll sideways on a phone: intended for code, but
    // worth a look when they hold a table's actions.
    const sideways: string[] = [];
    for (const el of Array.from(document.body.querySelectorAll('*'))) {
      const o = getComputedStyle(el).overflowX;
      if ((o === 'auto' || o === 'scroll') && el.scrollWidth > el.clientWidth + 1) {
        const id = el.getAttribute('data-testid') ?? el.getAttribute('data-slot') ?? '';
        const text = (el.textContent ?? '').trim().slice(0, 40);
        sideways.push(
          `<${el.tagName.toLowerCase()}${id ? ` ${id}` : ''}> ${el.scrollWidth}px in ${el.clientWidth}px "${text}"`,
        );
      }
    }
    return { width, scroll, culprits, sideways: sideways.slice(0, 5) };
  });
  if (found.scroll > found.width || found.culprits.length > 0) {
    note(
      `layout: ${file}: page ${found.scroll}px wide in a ${found.width}px viewport; ${found.culprits.join('; ') || 'no single element'}`,
    );
  }
  if (found.sideways.length > 0) {
    note(`layout: ${file}: scrolls sideways inside ${found.sideways.join('; ')}`);
  }
}

/**
 * One screenshot: `NN-<viewport>-<scheme>-<slug>.png`. The page is switched
 * to the scheme and back to light. Password fields are always masked.
 */
async function shot(
  who: Who,
  id: string,
  slug: string,
  what: string,
  options: { scheme?: Scheme; fullPage?: boolean; clip?: Locator } = {},
) {
  const scheme = options.scheme ?? 'light';
  const page = who.page;
  if (scheme !== 'light') await page.emulateMedia({ colorScheme: scheme });
  // Let theme transitions and dialog fade-ins finish (infinite spinners excepted).
  await page.waitForFunction(() =>
    document
      .getAnimations()
      .every(
        (a) => a.playState !== 'running' || a.effect?.getComputedTiming().endTime === Infinity,
      ),
  );
  const file = `${id}-${who.viewport}-${scheme}-${slug}.png`;
  const mask = [page.locator('input[type="password"]')];
  const fullPage = !options.clip && (options.fullPage ?? true);
  // A full-page capture of a scrolled page draws the sticky top bar mid-page.
  if (fullPage) await page.evaluate(() => window.scrollTo(0, 0));
  if (options.clip) {
    await options.clip.screenshot({ path: join(OUT, file), mask });
  } else {
    await page.screenshot({
      path: join(OUT, file),
      fullPage,
      mask,
    });
  }
  await checkOverflow(who, file);
  if (scheme !== 'light') await page.emulateMedia({ colorScheme: 'light' });
  index.push({ file, what: `${who.label}, ${who.viewport} ${scheme}: ${what}` });
}

function section(page: Page, stepRunId: string) {
  return page.locator(`section[data-step-id="${stepRunId}"]`);
}

function actions(page: Page, stepRunId: string) {
  return page.getByTestId(`actions-${stepRunId}`);
}

/** A raw user id on screen: the page should show names (walkthrough #1). */
const RAW_USER_ID = /usr_[0-9A-Z]{26}/;

/** The step headings in page order, without their numbers. */
async function sectionTitles(page: Page): Promise<string[]> {
  return page
    .locator('section[data-slot="step-section"] h2')
    .evaluateAll((headings) =>
      headings.map((h) => (h.textContent ?? '').replace(/^\s*\d+\.\s*/, '').trim()),
    );
}

/** Walkthrough #2: iterations sit under their container, in runbook order. */
async function expectRunbookOrder(page: Page) {
  const titles = await sectionTitles(page);
  const at = (title: string) => {
    const index = titles.indexOf(title);
    expect(index, `"${title}" in ${JSON.stringify(titles)}`).toBeGreaterThanOrEqual(0);
    return index;
  };
  const order = [
    'Error rate per region',
    'Error rate in us-west',
    'Error rate in us-east',
    'Error rate in eu-west',
    'Wait for errors to settle',
    'Re-check the global error rate',
    'Keep or roll back',
    'Roll back or keep?',
  ].map(at);
  expect(order, JSON.stringify(titles)).toEqual([...order].sort((x, y) => x - y));
}

async function stepRunOf(stack: E2EState, exe: string, path: string): Promise<string> {
  let sr: string | undefined;
  await waitFor(async () => {
    sr = await sectionStepRun(stack, 'alice', exe, path);
    return Boolean(sr);
  }, `section ${path}`);
  return sr as string;
}

/**
 * The agent session's CLI (the config dir AgentDriver.start set up), for the
 * one step this walkthrough shows claimed before it is reported.
 */
function agentCli(stack: E2EState, exe: string, args: string[]): Promise<void> {
  const env: NodeJS.ProcessEnv = { ...process.env };
  delete env.RUNBOOK_TOKEN;
  delete env.RUNBOOK_HOST;
  env.XDG_CONFIG_HOME = join(stack.pilotDir, 'config', `agent-${exe}`);
  return new Promise((ok, fail) => {
    execFile(stack.cli, [...args, '--host', stack.apiURL], { env }, (error, _out, stderr) => {
      if (error) fail(new Error(`agent: runbook ${args.slice(0, 2).join(' ')}: ${stderr}`));
      else ok();
    });
  });
}

async function fillStartDialog(page: Page) {
  const dialog = page.getByRole('dialog', { name: 'Start a run' });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByLabel('Runbook').locator('option:checked')).toHaveText(
    'Post-deploy verification',
  );
  const inputsSet = dialog.getByRole('group', { name: 'Inputs' });
  await inputsSet.getByRole('combobox', { name: /service/ }).click();
  await page.getByRole('option', { name: 'api', exact: true }).click();
  await inputsSet.getByRole('textbox', { name: /version/ }).fill('v2.0.0');
  await inputsSet.getByRole('button', { name: 'Save inputs' }).click();
  await expect(dialog.getByText('Inputs saved.')).toBeVisible();
  const fieldsSet = dialog.getByRole('group', { name: 'Fields' });
  await fieldsSet.getByRole('button', { name: 'Save fields' }).click();
  await expect(dialog.getByText('Fields saved.')).toBeVisible();
  return dialog;
}

/** Scrolls the start dialog (and anything scrollable in it) back to its top. */
async function dialogTop(page: Page) {
  await page.getByRole('dialog', { name: 'Start a run' }).evaluate((el) => {
    for (let n: Element | null = el; n; n = n.parentElement) n.scrollTop = 0;
    el.querySelectorAll('*').forEach((c) => (c.scrollTop = 0));
  });
}

async function tick(page: Page, panel: Locator, exe: string, item: string) {
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

async function writeOutputs() {
  const mode = process.env.E2E_MODE ?? 'native';
  await writeFile(
    join(OUT, 'INDEX.md'),
    [
      '# Flagship walkthrough: screenshots',
      '',
      `Written by \`web/e2e/walkthrough.spec.ts\` (\`npm run e2e:walkthrough\`, ${mode} mode). Numbers follow the walkthrough steps; a letter is a later state of the same step. Every person is a fictional pilot identity.`,
      '',
      ...[...index]
        .sort((x, y) => x.file.localeCompare(y.file))
        .map(({ file, what }) => `- \`${file}\`: ${what}`),
      '',
    ].join('\n'),
  );
  await writeFile(
    join(OUT, 'walkthrough-log.txt'),
    [
      `# Browser-side problems during the walkthrough (${mode} mode, ${new Date().toISOString()})`,
      '# console errors and warnings, page errors, failed requests, responses >= 400 (paths only),',
      '# and phone layouts wider than the viewport. One line each, with the walkthrough step.',
      ...(log.length > 0 ? log : ['(none)']),
      '',
    ].join('\n'),
  );
}

test.describe('flagship walkthrough with screenshots', { tag: '@walkthrough' }, () => {
  const opened: Who[] = [];

  test.beforeAll(async () => {
    await mkdir(OUT, { recursive: true });
    for (const name of await readdir(OUT)) {
      if (OWN_FILE.test(name)) await rm(join(OUT, name));
    }
  });

  test.afterAll(async () => {
    for (const who of opened) await who.context.close();
    await writeOutputs();
  });

  test('as Alice, Bob and Carol on desktop and phone', async ({ browser, stack }) => {
    test.setTimeout(420_000);
    const open = async (label: string, viewport: Viewport, file: string) => {
      const who = await openPerson(browser, stack, label, viewport, file);
      opened.push(who);
      return who;
    };
    const step = (id: string, title: string, body: () => Promise<void>) =>
      test.step(`${id} ${title}`, async () => {
        currentStep = `${id} ${title}`;
        await body();
      });

    await step('01', 'sign-in page', async () => {
      for (const viewport of ['desktop', 'phone'] as const) {
        const who = await newContext(browser, stack, 'Signed out', viewport);
        opened.push(who);
        await who.page.goto('/sign-in');
        await expect(who.page.getByLabel('Personal access token')).toBeVisible();
        await expect(who.page.getByLabel('Personal access token')).toHaveValue('');
        // #13: the CLI command is a code block that wraps between words, with a copy button.
        const command = who.page.getByTestId('token-command');
        await expect(command.getByRole('button', { name: 'Copy command' })).toBeVisible();
        expect(
          await command.locator('code').evaluate((el) => {
            const style = getComputedStyle(el);
            return [style.wordBreak, style.overflowWrap, style.whiteSpace];
          }),
        ).toEqual(['normal', 'normal', 'pre-wrap']);
        // No word is split across lines (not even after "--").
        expect(
          await command
            .locator('[data-slot="command-words"] .whitespace-nowrap')
            .evaluateAll((words) => words.filter((w) => w.getClientRects().length > 1).length),
        ).toBe(0);
        for (const scheme of ['light', 'dark'] as const) {
          await shot(who, '01', 'signin', 'the sign-in page, empty', { scheme });
        }
      }
    });

    const alice = await open('Alice', 'desktop', browserTokenFile(stack, 'alice'));
    const alicePhone = await open('Alice', 'phone', browserTokenFile(stack, 'alice'));
    const bob = await open('Bob', 'desktop', browserTokenFile(stack, 'bob'));
    const bobPhone = await open('Bob', 'phone', browserTokenFile(stack, 'bob'));
    const carol = await open('Carol', 'desktop', browserTokenFile(stack, 'carol'));
    const carolPhone = await open('Carol', 'phone', browserTokenFile(stack, 'carol'));
    const a = alice.page;

    await step('02', 'My tasks, empty', async () => {
      for (const who of [alice, alicePhone]) {
        await expect(who.page.getByText('Inbox is clear')).toBeVisible();
        await shot(who, '02', 'tasks-empty', 'My tasks with nothing assigned (empty state)');
      }
    });

    await step('03', 'runbooks list', async () => {
      for (const who of [alice, alicePhone]) {
        await who.page.goto('/p/pilot/runbooks');
        await expect(who.page.getByText('Post-deploy verification').first()).toBeVisible();
        await shot(who, '03', 'runbooks', 'the project’s runbook list');
      }
    });

    let exe = '';
    await step('04', 'start dialog, filled', async () => {
      // Phone: through ⌘K → Start a run, filled, then closed without starting.
      const p = alicePhone.page;
      await p.getByRole('button', { name: 'Open command palette' }).click();
      await p.getByRole('dialog').getByRole('option', { name: 'Start a run' }).click();
      const phoneDialog = await fillStartDialog(p);
      await dialogTop(p);
      await shot(
        alicePhone,
        '04a',
        'start-dialog-top',
        'the start dialog, top (runbook, version)',
        {
          fullPage: false,
        },
      );
      await phoneDialog.getByRole('button', { name: 'Start run' }).scrollIntoViewIfNeeded();
      await shot(alicePhone, '04b', 'start-dialog', 'the start dialog filled (api, v2.0.0)', {
        fullPage: false,
      });
      await p.keyboard.press('Escape');
      await expect(phoneDialog).toBeHidden();

      // Desktop: the Start a run button on the runbook list; this one starts.
      await a.getByRole('button', { name: 'Start a run' }).click();
      const dialog = await fillStartDialog(a);
      await dialogTop(a);
      await shot(alice, '04a', 'start-dialog-top', 'the start dialog, top (runbook, version)', {
        fullPage: false,
      });
      await dialog.getByRole('button', { name: 'Start run' }).scrollIntoViewIfNeeded();
      await shot(alice, '04b', 'start-dialog', 'the start dialog filled (api, v2.0.0)', {
        fullPage: false,
      });
      await dialog.getByRole('button', { name: 'Start run' }).click();
      await expect(a).toHaveURL(/\/x\/exe_[0-9A-Z]+$/);
      exe = /\/x\/(exe_[0-9A-Z]+)$/.exec(a.url())?.[1] as string;
    });

    await step('05', 'the live document right after start', async () => {
      await expect(a.getByRole('heading', { name: 'Verify api v2.0.0' })).toBeVisible();
      await alicePhone.page.goto(`/x/${exe}`);
      for (const who of [alice, alicePhone]) {
        await expect(who.page.getByRole('heading', { name: 'Verify api v2.0.0' })).toBeVisible();
        await expect(who.page.getByRole('status').filter({ hasText: /^Live$/ })).toBeVisible();
        // #8: a queued run says so; once started, steps show. Never "Steps 0" over nothing.
        await expect(
          who.page
            .getByText('Queued: waiting for a slot')
            .or(who.page.locator('section[data-slot="step-section"]').first()),
        ).toBeVisible();
        await shot(who, '05', 'run-started', 'the execution document just after start (Live)');
      }
    });

    const agent = await AgentDriver.start(stack, exe);
    const health = await stepRunOf(stack, exe, 'api_health');
    const login = await stepRunOf(stack, exe, 'login_flow');

    await step('06', 'agent steps progressing', async () => {
      // The agent claims api_health and holds it before reporting.
      const sr = await agent.waitReady('api_health');
      expect(sr).toBe(health);
      await agentCli(stack, exe, [
        'step',
        'claim',
        sr,
        '--session',
        agent.session,
        '--agent-name',
        'e2e-agent',
        '--model',
        'manual',
      ]);
      await agentCli(stack, exe, ['step', 'get', sr]);
      for (const who of [alice, alicePhone]) {
        await expect(section(who.page, health).getByText('Claimed').first()).toBeVisible();
        await section(who.page, health).scrollIntoViewIfNeeded();
        await shot(who, '06a', 'agent-claimed', 'api_health claimed by the agent, not reported');
      }
      await agentCli(stack, exe, [
        'step',
        'report',
        sr,
        '--status',
        'succeeded',
        '--verdict',
        'pass',
        '--output',
        'status_code=200',
        '--summary',
        'GET /healthz returned 200',
      ]);
      for (const who of [alice, alicePhone]) {
        await expect(section(who.page, health).getByText('Succeeded')).toBeVisible();
        await shot(
          who,
          '06b',
          'agent-reported',
          'api_health reported (pass, 200); login_flow is next',
        );
      }
    });

    await step('07', 'Alice’s checklist on the phone', async () => {
      const p = alicePhone.page;
      const panel = actions(p, login);
      await panel.getByRole('button', { name: 'Claim this step' }).click();
      await expect(panel.getByRole('group', { name: 'Checklist form' })).toBeVisible();
      // #6: while claimed, the checklist shows once, as the form.
      await expect(section(p, login).locator('[data-slot="step-checklist"]')).toHaveCount(0);
      await section(p, login).scrollIntoViewIfNeeded();
      await shot(alicePhone, '07a', 'checklist-claimed', 'login_flow claimed by Alice', {
        fullPage: false,
      });
      for (const item of ['Login page loads', 'Dashboard shows data']) {
        await tick(p, panel, exe, item);
      }
      await panel.getByLabel('Check verdict, if this step is a check').selectOption('pass');
      await panel.scrollIntoViewIfNeeded();
      await shot(
        alicePhone,
        '07b',
        'checklist-ticked',
        'both items ticked, verdict pass, before Done',
        {
          fullPage: false,
        },
      );
      await panel.getByRole('button', { name: 'Done' }).click();
      await expect(section(p, login).getByText('Succeeded')).toBeVisible();
      await section(p, login).scrollIntoViewIfNeeded();
      await shot(alicePhone, '07c', 'checklist-reported', 'login_flow reported: Succeeded', {
        fullPage: false,
      });
      await expect(section(a, login).getByText('Succeeded')).toBeVisible();
      await shot(alice, '07c', 'checklist-reported', 'the same run on Alice’s desktop, live');
    });

    let signoff = '';
    await step('06', 'agent steps progressing', async () => {
      await agent.regionsUnhealthy();
      signoff = await stepRunOf(stack, exe, 'signoff');
      await expect(actions(a, signoff).getByText('Approval required')).toBeVisible();
      for (const who of [alice, alicePhone]) {
        await expect(actions(who.page, signoff).getByText('Approval required')).toBeVisible();
        await shot(
          who,
          '06c',
          'agent-regions',
          'the agent reported three regions (eu-west fails) and the re-check; the approval opened',
        );
      }
      // #2: the region checks and the re-check sit under their containers.
      await expectRunbookOrder(a);
      // #4: the switch waiting on its approval reads Running, not Ready.
      const decide = await stepRunOf(stack, exe, 'decide');
      await expect(section(a, decide).getByText('Running')).toBeVisible();
      await expect(section(a, decide).getByText('Ready')).toHaveCount(0);
      // #1: names, not raw user ids: Alice on her step, the eligible list.
      await expect(section(a, login).getByText('Alice')).toBeVisible();
      await expect(section(a, login)).not.toContainText(RAW_USER_ID);
      await expect(actions(a, signoff)).not.toContainText(RAW_USER_ID);
      await expect(actions(a, signoff)).toContainText(/Eligible: .*Alice/);
      // #10: Alice may decide this approval; a live event refreshes her count.
      const aliceCount = await expect
        .poll(() => navTaskCount(a), { timeout: 10_000 })
        .toBeGreaterThan(0)
        .then(
          () => 1,
          () => 0,
        );
      if (aliceCount === 0) {
        note(
          'Alice (desktop): sidebar My tasks shows 0 while an approval she may decide is open on the page she is reading',
        );
      }
      expect(aliceCount, 'Alice’s My tasks count follows the live run').toBe(1);
    });

    await step('08', 'My tasks for Bob', async () => {
      for (const who of [bob, bobPhone]) {
        await who.page.goto('/tasks');
        await expect(who.page.getByRole('heading', { name: 'Approvals (1)' })).toBeVisible();
        await shot(who, '08', 'tasks-bob', 'Bob’s My tasks with the signoff approval');
      }
    });

    await step('13', 'Carol is not eligible', async () => {
      for (const who of [carol, carolPhone]) {
        await who.page.goto(`/x/${exe}`);
        const panel = actions(who.page, signoff);
        // #7: the notice sits inside the approval panel.
        await expect(
          panel
            .locator('[data-slot="approval-panel"]')
            .getByText('You are not eligible to decide this approval.'),
        ).toBeVisible();
        await panel.scrollIntoViewIfNeeded();
        await shot(
          who,
          '13',
          'not-eligible',
          'Carol on the approval: not eligible, buttons disabled',
          {
            fullPage: false,
          },
        );
      }
    });

    await step('09', 'Bob’s approval panel', async () => {
      const b = bob.page;
      await b.getByRole('button', { name: new RegExp(`Roll back or keep\\?.*${exe}`) }).click();
      await expect(b).toHaveURL(new RegExp(`/x/${exe}$`));
      const panel = actions(b, signoff);
      await expect(panel.getByText('Approval required')).toBeVisible();
      await panel.scrollIntoViewIfNeeded();
      // #3: scrolled down the document, the rail stays on screen with the current step lit.
      const rail = b.getByRole('navigation', { name: 'Steps navigation' });
      await expect(rail).toBeInViewport();
      await expect(rail.locator('[aria-current="step"]')).toBeInViewport();
      await expect(rail.locator('[aria-current="step"]')).toContainText('Roll back or keep?');
      for (const scheme of ['light', 'dark'] as const) {
        await shot(bob, '09a', 'approval', 'Bob’s approval panel, open', {
          scheme,
          fullPage: false,
        });
      }
      await bobPhone.page.goto(`/x/${exe}`);
      const phonePanel = actions(bobPhone.page, signoff);
      await expect(phonePanel.getByText('Approval required')).toBeVisible();
      await phonePanel.scrollIntoViewIfNeeded();
      await shot(bobPhone, '09a', 'approval', 'Bob’s approval panel, open', { fullPage: false });

      await panel
        .getByLabel('Decision comment (optional)')
        .fill('Recovered after the re-check; keep v2.0.0');
      await panel.getByRole('button', { name: 'keep' }).click();
      const confirm = b.getByRole('alertdialog', { name: 'Confirm: keep' });
      await expect(confirm).toBeVisible();
      for (const scheme of ['light', 'dark'] as const) {
        await shot(bob, '09b', 'approval-confirm', 'Bob confirms keep (with a comment)', {
          scheme,
          fullPage: false,
        });
      }
      await confirm.getByRole('button', { name: 'keep' }).click();
      await expect(panel.getByText('chose keep')).toBeVisible();
      // #5: the outcome, by name, and no call to action.
      await expect(panel.getByText('Decided: keep by Bob')).toBeVisible();
      await expect(panel).not.toContainText('Choose an action');
      await expect(panel).not.toContainText('Approval required');
      // #10: his vote refreshes his count.
      await expect
        .poll(() => navTaskCount(b), { timeout: 10_000 })
        .toBe(0)
        .catch(async () => {
          note(
            `Bob (desktop): sidebar My tasks still shows ${await navTaskCount(b)} after his own vote closed the approval`,
          );
          expect(await navTaskCount(b), 'Bob’s My tasks count after his vote').toBe(0);
        });
      await shot(bob, '09c', 'approval-decided', 'Bob’s page after his vote: chose keep', {
        fullPage: false,
      });
    });

    await step('10', 'Alice’s page updated live', async () => {
      for (const who of [alice, alicePhone]) {
        await expect(actions(who.page, signoff).getByText('chose keep')).toBeVisible({
          timeout: 5_000,
        });
        await expect(section(who.page, signoff).getByText('Succeeded')).toBeVisible();
        // #1 and #5: the outcome and decided_by by name.
        await expect(actions(who.page, signoff).getByText('Decided: keep by Bob')).toBeVisible();
        await expect(section(who.page, signoff)).not.toContainText(RAW_USER_ID);
        await expect(actions(who.page, signoff)).not.toContainText(RAW_USER_ID);
        await actions(who.page, signoff).scrollIntoViewIfNeeded();
        await shot(
          who,
          '10',
          'live-decision',
          'Alice’s open page shows Bob’s decision without a reload',
          {
            fullPage: false,
          },
        );
      }
    });

    await step('11', 'the completed run', async () => {
      await agent.doStep('notify', { summary: 'Report posted to #deploys' });
      for (const who of [alice, alicePhone]) {
        const page = who.page;
        await expect(page.getByRole('status').filter({ hasText: 'Completed' })).toBeVisible();
        // #9: "Completed" and the header come from one document read: they agree at once.
        const header = page.locator('[data-slot="document-header"]');
        const seenAt = Date.now();
        if (!(await header.getByText('Succeeded').first().isVisible())) {
          const pill = (await header.first().innerText()).match(/Queued|Running|Waiting|Paused/);
          await expect(header.getByText('Succeeded').first()).toBeVisible();
          note(
            `${who.label} (${who.viewport}): the page said "Completed" while the run header still said "${pill?.[0] ?? '?'}"; Succeeded ${Date.now() - seenAt} ms later`,
          );
          expect
            .soft(false, 'the header said Succeeded as soon as the page said Completed')
            .toBe(true);
        }
        await expectRunbookOrder(page);
        await shot(who, '11', 'run-completed', 'the completed run report (Succeeded)');
      }
    });

    await step('12', 'the Markdown export', async () => {
      const download = a.waitForEvent('download');
      await a.getByRole('button', { name: 'Download Markdown' }).click();
      const saved = await (await download).path();
      await copyFile(saved, join(OUT, '12-export-verify-api-v2.0.0.md'));
      index.push({
        file: '12-export-verify-api-v2.0.0.md',
        what: 'the Markdown the page downloads (Download Markdown), as saved',
      });
      const web = await readFile(saved, 'utf8');
      expect
        .soft(web, 'the download equals `runbook run report --format md`')
        .toBe(await cliMarkdownReport(stack, 'alice', exe));
    });

    // The editor (Carol, project role editor) and the versions diff.
    const editorToken = join(stack.tokensDir, `${stack.editor}.editor`);
    const editor = await open('Carol (editor)', 'desktop', editorToken);
    const editorPhone = await open('Carol (editor)', 'phone', editorToken);
    const catalog = (await (
      await apiGet(stack, editorToken, `/v1/projects/${stack.projectId}/runbooks`)
    ).json()) as { items: { runbook_id: string; slug: string }[] };
    const runbookId = catalog.items.find((r) => r.slug === 'post-deploy')?.runbook_id ?? '';
    expect(runbookId).not.toBe('');

    await step('14', 'the editor', async () => {
      for (const who of [editor, editorPhone]) {
        const e = who.page;
        await e.goto(`/p/pilot/runbooks/${runbookId}/editor`);
        await expect(e.getByRole('heading', { level: 1, name: 'Editor' })).toBeVisible();
        await expect(e.getByTestId('yaml-editor').locator('.monaco-editor')).toBeVisible();
        await expect(e.getByTestId('step-budget')).toContainText('Step budget: at most');
        await shot(who, '14a', 'editor-yaml', 'the flagship draft in the YAML tab', {
          fullPage: false,
        });
        // Break the execution timeout: the browser compiler flags it as you type.
        await e
          .getByTestId('yaml-editor')
          .locator('.view-line')
          .filter({ hasText: /^\s*timeout:\s2h$/ })
          .first()
          .click();
        await e.keyboard.press('End');
        await e.keyboard.type('x');
        const diagnostics = e.getByTestId('diagnostics');
        await expect(diagnostics).toContainText('spec.invalid_value');
        // #11: a sentence first; the code once, as secondary text.
        // The compiler's own message (A16 catalog), not a client fallback.
        await expect(diagnostics).toContainText(
          'The value at /spec/execution/timeout is not allowed here.',
        );
        expect(
          (
            (await diagnostics.getByRole('listitem').first().innerText()).match(
              /spec\.invalid_value/g,
            ) ?? []
          ).length,
        ).toBe(1);
        await shot(who, '14b', 'editor-diagnostic', 'a provisional diagnostic (timeout: 2hx)', {
          fullPage: false,
        });
        await e.keyboard.press('Backspace');
        await expect(diagnostics.getByRole('heading', { level: 2 })).toHaveText(
          'Provisional diagnostics (0)',
        );
        for (const tab of ['Graph', 'Outline'] as const) {
          await e.getByRole('tab', { name: tab }).click();
          await expect(e.getByRole('tabpanel', { name: tab })).toBeVisible();
          if (tab === 'Graph') {
            // The graph is a lazy chunk: wait for drawn nodes, not the skeleton.
            const node = e.locator('[data-slot="runbook-graph"] .react-flow__node').first();
            await expect.soft(node, 'the graph draws its nodes').toBeVisible();
            if (!(await node.isVisible()))
              note(`${who.label} (${who.viewport}): the Graph tab drew no nodes`);
          } else {
            await expect(e.locator('[data-slot="runbook-outline"]')).toBeVisible();
          }
          await shot(
            who,
            tab === 'Graph' ? '14c' : '14d',
            `editor-${tab.toLowerCase()}`,
            `the ${tab} tab`,
            {
              fullPage: false,
            },
          );
        }
      }
      // A real change, saved and published as v2 for the diff.
      const e = editor.page;
      await e.getByRole('tab', { name: 'YAML' }).click();
      await e
        .getByTestId('yaml-editor')
        .locator('.view-line')
        .filter({ hasText: /^\s*description:\sVerify\sa\sproduction\sdeploy/ })
        .first()
        .click();
      await e.keyboard.press('End');
      await e.keyboard.type(' Edited in the browser.');
      await e.getByRole('button', { name: 'Save draft' }).click();
      await expect(e.getByTestId('editor-notice')).toHaveText(
        /^Saved revision \d+\. The server found no problems\.$/,
      );
      await e.getByRole('button', { name: 'Publish' }).click();
      await expect(e.getByTestId('editor-notice')).toHaveText('Published v2.');
      await shot(editor, '14e', 'editor-published', 'saved and published as v2', {
        fullPage: false,
      });
    });

    await step('15', 'the versions diff', async () => {
      for (const who of [editor, editorPhone]) {
        const e = who.page;
        await e.goto(`/p/pilot/runbooks/${runbookId}/versions`);
        await expect(e.getByLabel('Compare from')).toHaveValue('1');
        await expect(e.getByLabel('Compare to')).toHaveValue('2');
        await e.getByRole('button', { name: 'Compare versions' }).click();
        const diff = e.getByRole('region', { name: 'Diff 1 to 2' });
        await expect(diff).toContainText('Edited in the browser.');
        await shot(who, '15', 'versions-diff', 'versions v1 and v2 with the diff');
      }
    });

    await step('16', 'the ⌘K palette', async () => {
      await a.goto('/tasks');
      await expect(a.getByRole('heading', { name: 'My tasks' })).toBeVisible();
      await a.keyboard.press('ControlOrMeta+k');
      await expect(a.getByRole('option', { name: 'Start a run' })).toBeVisible();
      await shot(alice, '16a', 'palette', 'the command palette, opened with ⌘K', {
        fullPage: false,
      });
      await a.keyboard.type('verify');
      await shot(alice, '16b', 'palette-search', 'the palette filtered by “verify”', {
        fullPage: false,
      });
      await a.keyboard.press('Escape');
      const p = alicePhone.page;
      await p.goto('/tasks');
      await p.getByRole('button', { name: 'Open command palette' }).click();
      await expect(p.getByRole('option', { name: 'Start a run' })).toBeVisible();
      await shot(
        alicePhone,
        '16a',
        'palette',
        'the command palette, opened from the search button',
        {
          fullPage: false,
        },
      );
      await p.keyboard.press('Escape');
    });

    await step('17', 'the not-found page', async () => {
      for (const who of [alice, alicePhone]) {
        await who.page.goto('/no-such-page');
        await expect(who.page.getByRole('heading', { name: 'Page not found' })).toBeVisible();
        // #12: inside the app frame, with a way back to My tasks.
        await expect(who.page.getByRole('link', { name: 'Go to My tasks' })).toBeVisible();
        await expect(
          who.viewport === 'desktop'
            ? who.page.getByRole('navigation', { name: 'Primary' })
            : who.page.getByRole('button', { name: 'Open navigation menu' }),
        ).toBeVisible();
        await shot(who, '17', 'not-found', 'an unknown URL, signed in');
      }
    });
    currentStep = '99 done';
  });
});
