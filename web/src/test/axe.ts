// axe-core for component tests (A11). Every component test should end with
// `await expectNoAxeViolations(container)`.
//
// Runs the WCAG 2.0–2.2 A/AA rule tags. jsdom has no layout engine, so
// color-contrast is off here; contrast is covered by scripts/check-contrast.mjs
// (A1) and by @axe-core/playwright in e2e (M6).
import axe from 'axe-core';

const WCAG_TAGS = ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22a', 'wcag22aa'];

export async function axeViolations(
  context: Element | Document = document,
  options: axe.RunOptions = {},
): Promise<axe.Result[]> {
  const results = await axe.run(context, {
    runOnly: { type: 'tag', values: WCAG_TAGS },
    resultTypes: ['violations'],
    ...options,
    rules: { 'color-contrast': { enabled: false }, ...options.rules },
  });
  return results.violations;
}

export function formatViolations(violations: axe.Result[]): string {
  return violations
    .map((v) => {
      const nodes = v.nodes.map((n) => `    ${n.target.join(' ')}`).join('\n');
      return `[${v.impact ?? 'unknown'}] ${v.id}: ${v.help}\n${nodes}`;
    })
    .join('\n');
}

export async function expectNoAxeViolations(
  context: Element | Document = document,
  options: axe.RunOptions = {},
): Promise<void> {
  const violations = await axeViolations(context, options);
  if (violations.length > 0) {
    throw new Error(
      `axe found ${violations.length} violation(s):\n${formatViolations(violations)}`,
    );
  }
}
