// Shared Playwright fixtures (M6): the seeded stack, sign-in, and pages
// for each pilot person. Import `test` and `expect` from here.
import AxeBuilder from '@axe-core/playwright';
import {
  test as base,
  expect,
  type Browser,
  type BrowserContext,
  type Page,
} from '@playwright/test';
import { appendFileSync } from 'node:fs';
import { join } from 'node:path';
import { loadState, readSecret, type E2EState, type Person } from './state.ts';

export { expect };

export const PHONE = { width: 390, height: 844 } as const;
export const NAMES: Record<Person, string> = { alice: 'Alice', bob: 'Bob', carol: 'Carol' };

/** The browser token file for a person (narrow human PAT minted by the harness). */
export function browserTokenFile(state: E2EState, who: Person): string {
  return join(state.tokensDir, `${who}.browser`);
}

/**
 * The controller's browser token (`stack.controller`, Alice): the browser
 * scopes plus executions:admin, so the run page offers pause, resume and
 * cancel to this project operator (D27).
 */
export function controlTokenFile(state: E2EState): string {
  return join(state.tokensDir, `${state.controller}.control`);
}

/**
 * Sign in through the form with the token in `tokenFile`. The secret goes
 * straight into the password field; it is never logged.
 */
export async function signInWithFile(page: Page, tokenFile: string) {
  await page.goto('/sign-in');
  await page.getByLabel('Personal access token').fill(readSecret(tokenFile));
  await page.getByRole('button', { name: 'Sign in' }).click();
}

/** Sign in as a person with their browser token and wait for My tasks. */
export async function signIn(page: Page, state: E2EState, who: Person) {
  await signInWithFile(page, browserTokenFile(state, who));
  await expect(page).toHaveURL(/\/tasks$/);
  await expect(page.getByRole('heading', { name: 'My tasks' })).toBeVisible();
}

export interface PersonPage {
  context: BrowserContext;
  page: Page;
}

/** A fresh browser context (own sessionStorage) signed in as `who`. */
export async function openAs(
  browser: Browser,
  state: E2EState,
  who: Person,
  options: { viewport?: { width: number; height: number }; hasTouch?: boolean } = {},
): Promise<PersonPage> {
  const context = await browser.newContext({
    baseURL: state.baseURL,
    ...(options.viewport ? { viewport: options.viewport } : {}),
    ...(options.hasTouch ? { hasTouch: true } : {}),
  });
  recordBrowserIssues(context);
  const page = await context.newPage();
  await signIn(page, state, who);
  return { context, page };
}

/** The running test's title, for log lines from browser-wide sources. */
let currentTitle = '';

function appendLog(entry: Record<string, unknown>) {
  const file = process.env.E2E_BROWSER_LOG;
  if (file) appendFileSync(file, `${JSON.stringify({ test: currentTitle, ...entry })}\n`);
}

type CdpLogEntry = { source?: string; level?: string; text?: string };

/** Runs in each worker before its own code: report refusals through the console. */
const WORKER_LISTENER = `self.addEventListener('securitypolicyviolation', (e) => console.error('Content Security Policy violation in a worker: ' + e.effectiveDirective + ' blocked ' + e.blockedURI));`;

/**
 * A worker's CSP refusals reach neither Playwright console channel: Chromium
 * raises them only inside the worker (its \`securitypolicyviolation\` event,
 * a refused wasm compile among them) and in the worker's own security log.
 * So every page gets a CDP session that auto-attaches to the workers it
 * starts while they wait for a debugger, installs a listener that reports
 * through console.error (which Playwright forwards), enables the worker's
 * Log domain, and only then lets the worker run: nothing it does is missed.
 */
async function watchWorkers(context: BrowserContext, page: Page) {
  const cdp = await context.newCDPSession(page);
  const send = (sessionId: string, id: number, method: string, params: object = {}) =>
    cdp
      .send('Target.sendMessageToTarget', {
        sessionId,
        message: JSON.stringify({ id, method, params }),
      })
      .catch(() => {}); // the worker ended first
  cdp.on(
    'Target.attachedToTarget',
    (event: { sessionId: string; targetInfo: { type: string }; waitingForDebugger: boolean }) => {
      const { sessionId } = event;
      void (async () => {
        await send(sessionId, 1, 'Runtime.evaluate', { expression: WORKER_LISTENER });
        await send(sessionId, 2, 'Log.enable');
        await send(sessionId, 3, 'Runtime.runIfWaitingForDebugger');
      })();
    },
  );
  cdp.on('Target.receivedMessageFromTarget', (event: { message: string }) => {
    let message: { method?: string; params?: { entry?: CdpLogEntry } };
    try {
      message = JSON.parse(event.message) as typeof message;
    } catch {
      return;
    }
    const entry = message.params?.entry;
    if (message.method !== 'Log.entryAdded' || !entry?.text) return;
    if (entry.source !== 'security' && !isCspRefusal(entry.text)) return;
    const text = entry.text.slice(0, 300);
    appendLog({ kind: 'worker.log', csp: true, text });
    cspRefusals.push(`worker: ${text.slice(0, 200)}`);
  });
  await cdp.send('Target.setAutoAttach', {
    autoAttach: true,
    waitForDebuggerOnStart: true,
    flatten: false,
  });
}

/**
 * CSP refusals seen in the current test, from any of its contexts. The
 * `browserIssues` fixture fails the test when any is left at its end (U12).
 */
const cspRefusals: string[] = [];
const recorded = new WeakSet<BrowserContext>();

/** True for a browser console message that reports a CSP refusal. */
export function isCspRefusal(text: string): boolean {
  return /Content Security Policy|Content-Security-Policy/i.test(text);
}

/**
 * Browser-side problems no spec asserts on: console errors and warnings
 * (CSP refusals among them), uncaught page errors, failed requests and
 * responses of 400 or more. With E2E_BROWSER_LOG set (the harness sets
 * `e2e/.run/browser-<mode>.jsonl`), each is one JSON line, so a container
 * run can be compared with a native one. Paths only: never a query string,
 * a header or a body.
 *
 * CSP refusals are caught three ways, each also recorded as `csp: true`:
 * the document's `securitypolicyviolation` event, the console message the
 * browser logs for the page, and every dedicated worker's own event and
 * security log (Monaco's and the compiler's; `watchWorkers`). Any of them
 * fails the test.
 */
export function recordBrowserIssues(context: BrowserContext) {
  if (recorded.has(context)) return;
  recorded.add(context);
  const watch = (page: Page) => {
    void watchWorkers(context, page).catch(() => {}); // a page closed during setup
  };
  context.pages().forEach(watch);
  context.on('page', watch);
  const file = process.env.E2E_BROWSER_LOG;
  let title = '';
  try {
    title = test.info().titlePath.slice(1).join(' › ');
  } catch {
    // outside a test (a hook without test info): keep it untitled
  }
  const write = (entry: Record<string, unknown>) => {
    if (file) appendFileSync(file, `${JSON.stringify({ test: title, ...entry })}\n`);
  };
  const path = (url: string) => {
    try {
      const parsed = new URL(url);
      return parsed.protocol === 'http:' || parsed.protocol === 'https:'
        ? parsed.pathname
        : `${parsed.protocol}…`;
    } catch {
      return url.slice(0, 80);
    }
  };
  const consoleEntry = (kind: string, text: string) => {
    // Classify on the full text: a long URL could push the directive past the cut.
    const csp = isCspRefusal(text);
    // Inline data: URIs (fonts) would push the CSP directive past the cut.
    const short = text.replace(/data:[^'"\s)]{40,}/g, (uri) => `${uri.slice(0, 30)}…`);
    write({ kind, text: short.slice(0, 300), ...(csp ? { csp: true } : {}) });
    if (csp) cspRefusals.push(`${kind}: ${short.slice(0, 200)}`);
  };
  context.on('console', (message) => {
    if (message.type() !== 'error' && message.type() !== 'warning') return;
    consoleEntry(`console.${message.type()}`, message.text());
  });
  // The DOM event names the directive even when the console text changes.
  void context
    .exposeBinding(
      '__e2eCspViolation',
      (_source, v: { directive: string; blocked: string; source: string }) => {
        const entry = { directive: v.directive, blocked: path(v.blocked), source: path(v.source) };
        write({ kind: 'csp', csp: true, ...entry });
        cspRefusals.push(`securitypolicyviolation: ${entry.directive} blocked ${entry.blocked}`);
      },
    )
    .catch(() => {});
  void context
    .addInitScript(() => {
      document.addEventListener('securitypolicyviolation', (e) => {
        const report = (
          window as unknown as {
            __e2eCspViolation?: (v: { directive: string; blocked: string; source: string }) => void;
          }
        ).__e2eCspViolation;
        void report?.({
          directive: e.effectiveDirective,
          blocked: e.blockedURI,
          source: e.sourceFile,
        });
      });
    })
    .catch(() => {});
  context.on('weberror', (error) => {
    write({ kind: 'pageerror', text: error.error().message.slice(0, 300) });
  });
  context.on('requestfailed', (request) => {
    write({
      kind: 'requestfailed',
      method: request.method(),
      path: path(request.url()),
      error: request.failure()?.errorText ?? '',
    });
  });
  context.on('response', (response) => {
    if (response.status() < 400) return;
    write({
      kind: 'response',
      method: response.request().method(),
      path: path(response.url()),
      status: response.status(),
    });
  });
}

/** The Inbox (My tasks) count in the primary navigation (desktop sidebar), or 0 when absent. */
export async function navTaskCount(page: Page): Promise<number> {
  const link = page
    .getByRole('navigation', { name: 'Primary' })
    .getByRole('link', { name: /Inbox/ });
  const text = (await link.first().innerText()).replace('Inbox', '').trim();
  return text === '' ? 0 : Number(text);
}

/** UI-1: sign-out sits in the account menu at the sidebar's foot. */
export async function signOutFromShell(page: Page): Promise<void> {
  await page.getByRole('button', { name: /^Account:/ }).click();
  await page.getByRole('button', { name: 'Sign out' }).click();
}

export type AxeImpact = 'minor' | 'moderate' | 'serious' | 'critical';

/**
 * The impacts that fail a scan (U32: plan §6 "axe clean"): moderate and
 * above. `minor` is reported by E2E_AXE_REPORT but does not fail.
 */
export const GATED_IMPACTS: readonly AxeImpact[] = ['moderate', 'serious', 'critical'];

/**
 * Reviewed exceptions: axe findings someone has looked at and accepted (or
 * that track a bug with its own failing test). Route scans skip exactly these
 * nodes so they keep catching everything else. Every entry needs a reason;
 * delete it when the finding is gone. Empty: nothing is excepted.
 */
export interface AxeException {
  rule: string;
  /** Nodes matching this CSS selector… */
  selector?: string;
  /** …or color-contrast nodes with exactly these computed colors (lowercase hex). */
  colors?: { fg: string; bg: string };
  /** Why it is accepted, or the test that tracks it. */
  reason: string;
}

export const AXE_EXCEPTIONS: AxeException[] = [];

export interface AxeFinding {
  id: string;
  impact: AxeImpact;
  help: string;
  targets: string[];
}

/**
 * axe on the current page, WCAG 2.x A/AA: returns moderate, serious and
 * critical violations (ids and target selectors only, so nothing sensitive
 * is kept). `exceptions: 'include'` keeps the AXE_EXCEPTIONS nodes too.
 * `include` scopes the scan to a selector, which must match an element: a
 * scan of nothing would pass whatever the page does.
 */
export async function axeViolations(
  page: Page,
  options: { exceptions?: 'skip' | 'include'; include?: string } = {},
): Promise<AxeFinding[]> {
  if (options.include) {
    const matched = await page.locator(options.include).count();
    if (matched === 0) {
      throw new Error(`axe include selector matches nothing: ${options.include}`);
    }
  }
  // Colors mid-transition (theme switch, dialog fade-in) are not what anyone
  // reads: let finite animations finish first. Spinners and pulses are infinite.
  await page.waitForFunction(() =>
    document
      .getAnimations()
      .every(
        (a) => a.playState !== 'running' || a.effect?.getComputedTiming().endTime === Infinity,
      ),
  );
  // @axe-core/playwright resolves its own playwright-core copy; the Page is the same at runtime.
  const axePage = page as unknown as ConstructorParameters<typeof AxeBuilder>[0]['page'];
  let builder = new AxeBuilder({ page: axePage }).withTags([
    'wcag2a',
    'wcag2aa',
    'wcag21a',
    'wcag21aa',
    'wcag22aa',
  ]);
  if (options.include) builder = builder.include(options.include);
  const results = await builder.analyze();
  const gated = (impact: string | null | undefined) => GATED_IMPACTS.includes(impact as AxeImpact);
  if (process.env.E2E_AXE_REPORT) {
    // Full detail for triage (selectors, snippets, colors); no secrets on these pages.
    const detail = results.violations.map((v) => ({
      id: v.id,
      impact: v.impact,
      gated: gated(v.impact),
      nodes: v.nodes.map((n) => ({
        target: n.target.join(' '),
        html: n.html.slice(0, 200),
        data: n.any[0]?.data ?? null,
      })),
    }));
    appendFileSync(
      process.env.E2E_AXE_REPORT,
      `${JSON.stringify({ test: test.info().titlePath.slice(1).join(' › '), url: new URL(page.url()).pathname, scheme: await page.evaluate(() => (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light')), detail })}\n`,
    );
  }
  const findings: AxeFinding[] = [];
  for (const v of results.violations) {
    if (!gated(v.impact)) continue;
    const nodes = [];
    for (const node of v.nodes) {
      if (options.exceptions !== 'include') {
        const data = (node.any[0]?.data ?? {}) as { fgColor?: string; bgColor?: string };
        let skip = false;
        for (const k of AXE_EXCEPTIONS.filter((issue) => issue.rule === v.id)) {
          if (k.colors) {
            skip ||= data.fgColor === k.colors.fg && data.bgColor === k.colors.bg;
          }
          if (k.selector) {
            skip ||= await page
              .locator(node.target.join(' '))
              .first()
              .evaluate((el, sel) => el.matches(sel), k.selector)
              .catch(() => false);
          }
        }
        if (skip) continue;
      }
      nodes.push(node);
    }
    if (nodes.length === 0) continue;
    findings.push({
      id: v.id,
      impact: v.impact as AxeImpact,
      help: v.help,
      targets: nodes.slice(0, 5).map((n) => n.target.join(' ')),
    });
  }
  return findings;
}

/** @deprecated The gate is now moderate and above: use `axeViolations`. */
export const seriousAxeViolations = axeViolations;

export const test = base.extend<{ stack: E2EState; browserIssues: void }>({
  // eslint-disable-next-line no-empty-pattern
  stack: async ({}, use) => {
    await use(loadState());
  },
  // The default context's share of recordBrowserIssues (openAs adds its
  // own); every test then fails on a CSP refusal in any of its contexts.
  browserIssues: [
    async ({ context }, use) => {
      cspRefusals.length = 0;
      currentTitle = test.info().titlePath.slice(1).join(' › ');
      recordBrowserIssues(context);
      await use();
      expect(cspRefusals, 'the browser refused resources under the CSP').toEqual([]);
    },
    { auto: true },
  ],
});
