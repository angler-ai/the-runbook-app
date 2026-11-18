// Accessibility (A11, M6): axe on every route and the main overlays in
// light and dark (zero moderate, serious or critical violations, U32), and a
// keyboard-only approval: Tab to the panel, choose an option, confirm.
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import {
  AgentDriver,
  apiGet,
  asPersonOk,
  sectionStepRun,
  startRun,
  waitFor,
} from './support/cli.ts';
import {
  axeViolations,
  browserTokenFile,
  expect,
  PHONE,
  signIn,
  test,
} from './support/fixtures.ts';
import { loadState, type Person } from './support/state.ts';

/** A post-deploy run on the unhealthy path, held at its open approval. */
let approvalExe = '';
let approvalStep = '';
let runbookId = '';

test.beforeAll(async () => {
  test.setTimeout(90_000);
  const stack = loadState();
  // A failed test restarts the worker and with it this hook: reuse the run
  // made by the first worker rather than queue a second `worker` run.
  const saved = join(stack.tokensDir, 'a11y-fixture.json');
  if (existsSync(saved)) {
    const fixture = JSON.parse(readFileSync(saved, 'utf8')) as {
      approvalExe: string;
      approvalStep: string;
      runbookId: string;
    };
    ({ approvalExe, approvalStep, runbookId } = fixture);
    return;
  }
  // The runbook ID for the catalog routes.
  const res = await apiGet(
    stack,
    browserTokenFile(stack, 'alice'),
    `/v1/projects/${stack.projectId}/runbooks`,
  );
  const catalog = (await res.json()) as { items: { runbook_id: string; slug: string }[] };
  runbookId = catalog.items.find((r) => r.slug === 'post-deploy')?.runbook_id ?? '';
  expect(runbookId).not.toBe('');

  // `worker` keeps this run apart from the flagship spec's `api` run
  // (post-deploy admits one run per service at a time).
  approvalExe = await startRun(stack, 'alice', { service: 'worker', version: 'v3.1.0' });
  const agent = await AgentDriver.start(stack, approvalExe);
  await agent.smoke();
  let login = '';
  await waitFor(async () => {
    login = (await sectionStepRun(stack, 'alice', approvalExe, 'login_flow')) ?? '';
    return login !== '';
  }, 'login_flow');
  await asPersonOk(stack, 'alice', ['step', 'claim', login, '--session', 'alice-cli']);
  await asPersonOk(stack, 'alice', [
    'step',
    'checklist',
    login,
    '--tick',
    'Login page loads',
    '--tick',
    'Dashboard shows data',
  ]);
  await asPersonOk(stack, 'alice', [
    'step',
    'report',
    login,
    '--status',
    'succeeded',
    '--verdict',
    'pass',
  ]);
  await agent.regionsUnhealthy();
  await waitFor(async () => {
    approvalStep = (await sectionStepRun(stack, 'bob', approvalExe, 'signoff')) ?? '';
    return approvalStep !== '';
  }, 'the signoff approval');
  writeFileSync(saved, JSON.stringify({ approvalExe, approvalStep, runbookId }), { mode: 0o600 });
});

interface RouteCase {
  name: string;
  /** Who is signed in, or null for a signed-out page. */
  as: Person | null;
  path: () => string;
  /** What must be on screen before axe runs. */
  ready: (page: Page) => Promise<void>;
}

const heading = (name: string | RegExp) => async (page: Page) => {
  await expect(page.getByRole('heading', { name, level: 1 }).first()).toBeVisible();
};

const ROUTES: RouteCase[] = [
  {
    name: 'sign-in',
    as: null,
    path: () => '/sign-in',
    ready: async (page) => {
      await expect(page.getByLabel('Personal access token')).toBeVisible();
    },
  },
  {
    name: 'sign-in after sign-out',
    as: null,
    path: () => '/sign-in?reason=signed-out',
    ready: async (page) => {
      await expect(page.getByRole('status')).toContainText('You have signed out');
    },
  },
  {
    name: 'my tasks (approval waiting)',
    as: 'bob',
    path: () => '/tasks',
    ready: async (page) => {
      await expect(page.getByRole('heading', { name: /^Approvals \(\d+\)$/ })).toBeVisible();
    },
  },
  {
    name: 'my tasks (empty)',
    as: 'carol',
    path: () => '/tasks',
    ready: async (page) => {
      await expect(page.getByText('Inbox is clear')).toBeVisible();
    },
  },
  { name: 'runbooks', as: 'alice', path: () => '/p/pilot/runbooks', ready: heading('Runbooks') },
  {
    name: 'runbook editor',
    as: 'alice',
    path: () => `/p/pilot/runbooks/${runbookId}/editor`,
    ready: heading('Editor'),
  },
  {
    name: 'runbook runs',
    as: 'alice',
    path: () => `/p/pilot/runbooks/${runbookId}/runs`,
    ready: async (page) => {
      await heading('Runs')(page);
      await expect(page.getByText('Verify worker v3.1.0').first()).toBeVisible();
    },
  },
  {
    name: 'runbook versions',
    as: 'alice',
    path: () => `/p/pilot/runbooks/${runbookId}/versions`,
    ready: async (page) => {
      await expect(page.getByText('v1').first()).toBeVisible();
    },
  },
  {
    name: 'executions',
    as: 'alice',
    path: () => '/p/pilot/executions',
    ready: async (page) => {
      await heading('Executions')(page);
      await expect(page.getByText('Verify worker v3.1.0').first()).toBeVisible();
    },
  },
  {
    name: 'execution document (approval open, eligible)',
    as: 'bob',
    path: () => `/x/${approvalExe}`,
    ready: async (page) => {
      await expect(
        page.getByTestId(`actions-${approvalStep}`).getByText('Approval required'),
      ).toBeVisible();
    },
  },
  {
    name: 'execution document (not eligible)',
    as: 'carol',
    path: () => `/x/${approvalExe}`,
    ready: async (page) => {
      await expect(page.getByText('You are not eligible to decide this approval.')).toBeVisible();
    },
  },
  {
    name: 'not found',
    as: 'alice',
    path: () => '/no-such-page',
    ready: heading('Page not found'),
  },
];

async function open(page: Page, route: RouteCase) {
  if (route.as) await signIn(page, loadState(), route.as);
  await page.goto(route.path());
  await route.ready(page);
}

for (const colorScheme of ['light', 'dark'] as const) {
  test.describe(`axe, ${colorScheme}`, () => {
    test.use({ colorScheme });

    for (const route of ROUTES) {
      test(`${route.name}`, async ({ page }) => {
        await open(page, route);
        expect(await page.evaluate(() => matchMedia('(prefers-color-scheme: dark)').matches)).toBe(
          colorScheme === 'dark',
        );
        expect(await axeViolations(page)).toEqual([]);
      });
    }

    test('command palette open', async ({ page, stack }) => {
      await signIn(page, stack, 'alice');
      await page.keyboard.press('ControlOrMeta+k');
      await expect(page.getByRole('option', { name: 'Start a run' })).toBeVisible();
      expect(await axeViolations(page)).toEqual([]);
    });

    test('start dialog open', async ({ page, stack }) => {
      await signIn(page, stack, 'alice');
      await page.goto('/p/pilot/runbooks');
      // Wait for the catalog so the dialog opens with a runbook selected (R54 covers the early case).
      await expect(page.getByText('Post-deploy verification').first()).toBeVisible();
      await page.getByRole('button', { name: 'Start a run' }).click();
      const dialog = page.getByRole('dialog', { name: 'Start a run' });
      await expect(dialog.getByRole('button', { name: 'Save inputs' })).toBeVisible();
      expect(await axeViolations(page)).toEqual([]);
    });

    test('phone navigation drawer open', async ({ page, stack }) => {
      await page.setViewportSize(PHONE);
      await signIn(page, stack, 'alice');
      await page.getByRole('button', { name: 'Open navigation menu' }).click();
      await expect(page.getByRole('dialog').getByRole('link', { name: /Inbox/ })).toBeVisible();
      expect(await axeViolations(page)).toEqual([]);
    });
  });
}

test('claimed human step with checklist and report bar, light and dark', async ({
  page,
  stack,
}) => {
  // A run of its own on `web` (one run per service at a time).
  const exe = await startRun(stack, 'alice', { service: 'web', version: 'v9.9.9' });
  await signIn(page, stack, 'alice');
  await page.goto(`/x/${exe}`);
  let login = '';
  await waitFor(async () => {
    login = (await sectionStepRun(stack, 'alice', exe, 'login_flow')) ?? '';
    return login !== '';
  }, 'login_flow');
  const panel = page.getByTestId(`actions-${login}`);
  await panel.getByRole('button', { name: 'Claim this step' }).click();
  await expect(panel.getByRole('group', { name: 'Checklist form' })).toBeVisible();
  for (const colorScheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme });
    expect(await axeViolations(page), colorScheme).toEqual([]);
  }
  // Leave Alice's My tasks as it was.
  await panel.getByRole('button', { name: 'Release claim' }).click();
  await expect(panel.getByRole('button', { name: 'Claim this step' })).toBeVisible();
});

test.describe('fixed accessibility issues (R56-R58)', () => {
  // Fixed (R56). Found by M6, axe serious, WCAG 2.5.3 Label in Name: the ⌘K
  // trigger in features/palette/PaletteHost.tsx renders the visible text
  // "Search ⌘K" but aria-label="Open command palette", so speech-input
  // users saying "click Search" miss it. axe: label-content-name-mismatch on
  // every signed-in page. Fix: drop the aria-label (the text already names
  // it) or make it start with "Search".
  test('the ⌘K trigger’s accessible name contains its visible text', async ({ page, stack }) => {
    await signIn(page, stack, 'alice');
    const findings = await axeViolations(page, {
      exceptions: 'include',
      include: 'button[aria-label="Search ⌘K (open command palette)"]',
    });
    expect(findings).toEqual([]);
  });

  // Fixed (R57). Found by M6, axe serious, WCAG 1.4.3: in the dark theme
  // --rb-color-text-muted (#94A3B8) on --rb-color-surface-raised (#334155)
  // is 4.03:1, under 4.5:1. It hits every table header, the inbox path
  // chips, sidebar counts, filter chips, palette hints, the document's input
  // labels and output keys (28 nodes across the routes). The A1 contrast
  // script (scripts/check-contrast.mjs) does not check this pair. Fix the
  // token in src/styles/tokens.css and add the pair to the script.
  test.describe(() => {
    test.use({ colorScheme: 'dark' });
    test('dark theme: muted text on raised surfaces reaches 4.5:1', async ({ page, stack }) => {
      await signIn(page, stack, 'alice');
      await page.goto('/p/pilot/runbooks');
      await expect(page.getByRole('columnheader', { name: 'Runbook' })).toBeVisible();
      expect(await axeViolations(page, { exceptions: 'include', include: 'thead' })).toEqual([]);
    });
  });

  // Fixed (R58). Found by M6, axe serious, WCAG 2.5.8 Target Size: the × "Clear
  // runbook filter" button in src/ui/lists/FilterBar.tsx (shown on a
  // runbook's Runs page) is smaller than 24×24 px and sits inside the input
  // without the spacing that would excuse it. Fix: give it size-6 (or more).
  test('the clear-filter button is at least 24×24 px', async ({ page, stack }) => {
    await signIn(page, stack, 'alice');
    await page.goto(`/p/pilot/runbooks/${runbookId}/runs`);
    const clear = page.getByRole('button', { name: 'Clear runbook filter' });
    await expect(clear).toBeVisible();
    expect(
      await axeViolations(page, {
        exceptions: 'include',
        include: 'button[aria-label="Clear runbook filter"]',
      }),
    ).toEqual([]);
  });
});

test.describe('keyboard only', () => {
  test('Bob decides the approval with the keyboard: Tab to the panel, choose, confirm', async ({
    page,
    stack,
  }) => {
    await signIn(page, stack, 'bob');
    await page.goto(`/x/${approvalExe}`);
    const panel = page.getByTestId(`actions-${approvalStep}`);
    await expect(panel.getByText('Approval required')).toBeVisible();
    // Start from the top of the document, as after a fresh load.
    await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());

    async function tabTo(target: ReturnType<Page['getByRole']>, limit = 150) {
      for (let i = 0; i < limit; i++) {
        if (await target.evaluate((el) => el === document.activeElement)) return;
        await page.keyboard.press('Tab');
      }
      throw new Error('Tab never reached the target');
    }

    const comment = panel.getByLabel('Decision comment (optional)');
    await tabTo(comment);
    await expect(comment).toBeFocused();
    await page.keyboard.type('Rolling back by keyboard');

    const rollback = panel.getByRole('button', { name: 'rollback' });
    await tabTo(rollback, 10);
    await expect(rollback).toBeFocused();
    await page.keyboard.press('Enter');

    const confirm = page.getByRole('alertdialog', { name: 'Confirm: rollback' });
    await expect(confirm).toBeVisible();
    const confirmButton = confirm.getByRole('button', { name: 'rollback' });
    await tabTo(confirmButton, 10);
    await expect(confirmButton).toBeFocused();
    await page.keyboard.press('Enter');

    await expect(confirm).toBeHidden();
    await expect(panel.getByText('chose rollback')).toBeVisible();
    await expect(panel.getByText('"Rolling back by keyboard"')).toBeVisible();
  });
});
