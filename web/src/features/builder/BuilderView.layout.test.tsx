// UI-4 review round 1: selecting a step (or dragging one) re-renders the
// builder; the graph must not be laid out again for that, only when the
// runbook changed.
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { tokenStore } from '../../api/token-store';
import { server } from '../../test/msw/server';
import { rememberEditorTab } from '../runbook/tabs';
import { editorHandlers, fakeCompiler, renderEditor } from '../editor/test-support';
import type * as LayoutModule from '../../ui/graph/layout';

const calls = vi.hoisted(() => ({ count: 0 }));
vi.mock('../../ui/graph/layout', async (importOriginal) => {
  const actual = await importOriginal<typeof LayoutModule>();
  return {
    ...actual,
    layout: (...args: Parameters<typeof actual.layout>) => {
      calls.count += 1;
      return actual.layout(...args);
    },
  };
});
vi.mock('../editor/YamlEditor', async () => ({
  default: (await import('../editor/test-support')).StubYamlEditor,
}));

const SOURCE = `metadata:\n  id: lay\nspec:\n  steps:\n    - id: a\n    - id: b\n    - id: c\n`;

beforeEach(() => {
  tokenStore.set('trb_pat_test');
  rememberEditorTab('graph');
});
afterEach(() => tokenStore.clear());

it('does not lay the graph out again when only the selection changes', async () => {
  server.use(...editorHandlers({ draft: { source: SOURCE, revision: 1 }, latestVersion: null }));
  renderEditor(fakeCompiler(() => []));
  const card = (id: string) =>
    document.querySelector<HTMLElement>(`[data-node-id="${id}"] [data-slot="node-select"]`);
  await waitFor(() => expect(card('c')).not.toBeNull());
  // The compiler's first answer (the step budget) changes the outline: wait for it.
  await waitFor(() => expect(screen.getByTestId('step-budget')).not.toHaveTextContent('checking'));
  await new Promise((resolve) => setTimeout(resolve, 50));
  const before = calls.count;
  for (const id of ['a', 'b', 'c', 'a']) {
    fireEvent.click(card(id)!);
    await waitFor(() => expect(document.querySelector('[data-slot="step-panel"]')).not.toBeNull());
  }
  expect(calls.count).toBe(before);
});
