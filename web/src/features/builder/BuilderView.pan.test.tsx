// UI-4b item 4: "Show <step>" in the problems list pans to the step in a
// read-only editor too (Git-managed, a read-only token, locked YAML). Its
// own file: React Flow needs a measured pane (stubbed below), and earlier
// graph tests in one module can leave d3's zoom state unusable.
import { screen, waitFor, within, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { tokenStore } from '../../api/token-store';
import { server } from '../../test/msw/server';
import type { Diagnostic } from '../../wasm/runbook';
import { NODE_HEIGHT, NODE_WIDTH } from '../../ui/graph/layout';
import { forgetGraphView } from '../../ui/graph/RunbookGraph';
import { rememberEditorTab } from '../runbook/tabs';
import {
  RB,
  editorHandlers,
  fakeCompiler,
  renderEditor,
  type EditorServer,
} from '../editor/test-support';

vi.mock('../editor/YamlEditor', async () => ({
  default: (await import('../editor/test-support')).StubYamlEditor,
}));

const STEPS = Array.from({ length: 12 }, (_, i) => `    - id: s${i}\n`).join('');
const SOURCE = `apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata:\n  id: long\nspec:\n  steps:\n${STEPS}`;
const LOCKED = `apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata:\n  id: long\nspec:\n  defaults: &d\n    actor: human\n  steps:\n${STEPS}`;

/** A problem on the last step (its card is titled from its ID: "S11"). */
const far: Diagnostic = {
  path: '/spec/steps/11',
  code: 'spec.example',
  message: 'Something about s11.',
  line: null,
  column: null,
};

let restore: (() => void)[] = [];
beforeEach(() => {
  tokenStore.set('trb_pat_test');
  rememberEditorTab('graph');
  // Each case starts from the top, not the view the last one left.
  forgetGraphView(`editor:${RB}`);
  // jsdom measures nothing: give the canvas a size so React Flow lays it out,
  // and ask for reduced motion so the pan is immediate (no d3 transition).
  const width = vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(800);
  const height = vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(400);
  const motion = vi.spyOn(window, 'matchMedia').mockImplementation(
    (query: string) =>
      ({
        matches: query === '(prefers-reduced-motion: reduce)',
        media: query,
        onchange: null,
        addListener: () => {},
        removeListener: () => {},
        addEventListener: () => {},
        removeEventListener: () => {},
        dispatchEvent: () => false,
      }) as MediaQueryList,
  );
  restore = [() => width.mockRestore(), () => height.mockRestore(), () => motion.mockRestore()];
});
afterEach(() => {
  restore.forEach((r) => r());
  tokenStore.clear();
});

/** Whether a card is inside the 800×400 pane, from the transforms. */
function inPane(id: string): boolean {
  const numbers = (selector: string) =>
    (document.querySelector<HTMLElement>(selector)?.style.transform.match(/-?[\d.]+/g) ?? []).map(
      Number,
    );
  const [vx = 0, vy = 0, zoom = 1] = numbers('.react-flow__viewport');
  const [nx = 0, ny = 0] = numbers(`.react-flow__node[data-id="${id}"]`);
  const left = vx + nx * zoom;
  const top = vy + ny * zoom;
  return (
    left >= 0 && top >= 0 && left + NODE_WIDTH * zoom <= 800 && top + NODE_HEIGHT * zoom <= 400
  );
}

describe('read-only editors pan to a step shown from the problems list', () => {
  it.each<[string, Partial<EditorServer>, string]>([
    ['a Git-managed runbook', { origin: 'git' }, SOURCE],
    ['a read-only token', { scopes: ['runbooks:read', 'skills:read'] }, SOURCE],
    ['YAML the builder cannot edit', {}, LOCKED],
  ])('in %s', async (_, overrides, source) => {
    server.use(
      ...editorHandlers({ draft: { source, revision: 2 }, latestVersion: 1, ...overrides }),
    );
    renderEditor(fakeCompiler(() => [far]));
    await waitFor(() => expect(document.querySelector('[data-node-id="s0"]')).not.toBeNull());
    expect(
      screen.queryByTestId('read-only') ?? screen.queryByTestId('builder-locked'),
    ).not.toBeNull();
    await waitFor(() => expect(inPane('s0')).toBe(true));
    expect(inPane('s11')).toBe(false);
    const problems = await waitFor(() =>
      within(screen.getByTestId('diagnostics')).getByRole('list', { name: 'Problems' }),
    );
    await userEvent.click(within(problems).getByRole('button', { name: 'Show S11' }));
    await waitFor(() => expect(inPane('s11')).toBe(true));
    // A step already in view stays where it is.
    fireEvent.click(
      document.querySelector('[data-node-id="s11"] [data-slot="node-select"]') as HTMLElement,
    );
    expect(inPane('s11')).toBe(true);
  });
});
