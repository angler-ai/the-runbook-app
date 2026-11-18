// UI-4: the builder in the editor, over MSW and a stand-in compiler: the
// palette, each kind's form, edits reaching the YAML, problems on the card
// and the field, delete/move/duplicate with focus and undo, two-way sync
// with the YAML tab, the read-only states, the draft conflict's reapply,
// and axe.
import { act, fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { parseDocument } from 'yaml';
import { tokenStore } from '../../api/token-store';
import { expectNoAxeViolations } from '../../test/axe';
import { server } from '../../test/msw/server';
import type { Diagnostic } from '../../wasm/runbook';
import { rememberEditorTab } from '../runbook/tabs';
import {
  RB,
  editorHandlers,
  etagOf,
  fakeCompiler,
  problem,
  renderEditor,
  type EditorServer,
} from '../editor/test-support';
import { layout } from '../../ui/graph/layout';
import { readBuilder } from './model';

vi.mock('../editor/YamlEditor', async () => ({
  default: (await import('../editor/test-support')).StubYamlEditor,
}));

const SOURCE = `# Release checks
metadata:
  id: release-checks
  title: Release checks
spec:
  steps:
    - id: health
      title: Health answers 200
      check: true
      x-note: keep me
    - id: approve
      approval:
        prompt: Ship it?
        options: [ship, hold]
        approvers: { teams: [oncall] }
    - id: canary
      title: Canary
      parallel:
        - id: canary_us
        - id: canary_eu
    - id: gate
      switch:
        cases:
          - when: "\${{ inputs.risk == 'high' }}"
            steps:
              - id: hold
                end: { status: cancelled }
    - id: regions
      foreach:
        items: "\${{ inputs.regions }}"
        as: region
        max_items: 5
        steps:
          - id: deploy
    - id: settle
      loop:
        until: "\${{ steps.recheck.verdict == 'pass' }}"
        max_iterations: 3
        steps:
          - id: recheck
            check: true
    - id: verify
      call:
        runbook: post-deploy-checks
    - id: signal
      wait: { signal: go }
    - id: record
      set: { fields: { outcome: done } }
    - id: tidy
      steps:
        - id: inner
`;

function state(overrides: Partial<EditorServer> = {}): EditorServer {
  return {
    draft: { source: SOURCE, revision: 4 },
    latestVersion: 2,
    teams: [{ id: 'team_1', slug: 'oncall' }],
    catalog: [
      {
        runbook_id: 'rb_child',
        slug: 'post-deploy-checks',
        title: 'Post-deploy checks',
        origin: 'ui',
        archived: false,
        latest_version: 3,
        tags: [],
        draft_ahead: false,
      },
    ],
    ...overrides,
  };
}

const yamlBox = () => screen.getByRole('textbox', { name: /Runbook YAML/ }) as HTMLTextAreaElement;
const yamlText = () => yamlBox().value;
const yamlData = () =>
  parseDocument(yamlText()).toJS() as { spec: { steps: Record<string, unknown>[] } };
const card = (id: string) => {
  const el = document.querySelector<HTMLElement>(`[data-node-id="${id}"]`);
  if (!el) throw new Error(`no card ${id}`);
  return el;
};
/** The open step panel (waits for it). */
const stepPanel = () =>
  waitFor(() => {
    const el = document.querySelector<HTMLElement>('[data-slot="step-panel"]');
    if (!el) throw new Error('no step panel');
    return el;
  });

async function openBuilder(overrides: Partial<EditorServer> = {}, diagnostics: Diagnostic[] = []) {
  server.use(...editorHandlers(state(overrides)));
  const utils = renderEditor(fakeCompiler(() => diagnostics));
  await waitFor(() => card('health'));
  return { user: userEvent.setup(), ...utils };
}

async function select(id: string) {
  fireEvent.click(card(id).querySelector('[data-slot="node-select"]')!);
  await waitFor(() => expect(document.querySelector('[data-slot="step-panel"]')).not.toBeNull());
  return document.querySelector<HTMLElement>('[data-slot="step-panel"]')!;
}

beforeEach(() => {
  tokenStore.set('trb_pat_test');
  rememberEditorTab('graph');
});
afterEach(() => tokenStore.clear());

describe('the builder: forms by kind', () => {
  it('edits a task’s fields straight into the YAML, keeping what it does not model', async () => {
    const { user } = await openBuilder();
    const form = await select('health');
    await waitFor(() =>
      expect(within(form).getByRole('heading', { name: 'Health answers 200' })).toHaveFocus(),
    );
    for (const label of [
      'Title',
      'ID',
      'Instructions',
      'Assignee',
      'Strict',
      'Check: ends with a verdict',
      'Runs only if',
      'Timeout',
      'On failure',
      'Attempts',
      'Destructive',
      'Skill',
    ])
      expect(within(form).getByLabelText(label)).toBeInTheDocument();
    expect(within(form).getByRole('group', { name: 'Who does it' })).toBeInTheDocument();
    expect(within(form).getByRole('group', { name: 'Outputs' })).toBeInTheDocument();
    expect(within(form).getByRole('group', { name: 'Evidence' })).toBeInTheDocument();

    await user.type(within(form).getByLabelText('Instructions'), 'Call it.');
    await user.click(within(form).getByRole('radio', { name: 'Person' }));
    await user.type(within(form).getByLabelText('Assignee'), 'team:oncall');
    await user.click(within(form).getByRole('checkbox', { name: 'Strict' }));
    await user.click(within(form).getByRole('checkbox', { name: 'Required evidence: link' }));
    await user.click(within(form).getByRole('button', { name: 'Add an output' }));
    await user.type(within(form).getByLabelText('Timeout'), '15m');

    const health = yamlData().spec.steps[0];
    expect(health).toEqual({
      id: 'health',
      title: 'Health answers 200',
      actor: 'human',
      assignee: 'team:oncall',
      assignment: 'strict',
      check: true,
      instructions: 'Call it.',
      outputs: { output: { type: 'string' } },
      evidence: { required: ['link'] },
      timeout: '15m',
      'x-note': 'keep me',
    });
    // The comment and every other step are as they were.
    expect(yamlText().startsWith('# Release checks\n')).toBe(true);
    expect(yamlText()).toContain('    - id: approve\n      approval:\n        prompt: Ship it?\n');
    // Clearing the assignee clears strict with it.
    await user.clear(within(form).getByLabelText('Assignee'));
    expect(yamlData().spec.steps[0]).not.toHaveProperty('assignment');
    await expectNoAxeViolations(document.querySelector('main')!);
  });

  it('edits skills on a task: 1 skill writes skill:, 2+ writes skills:, removing cleans up', async () => {
    const { user } = await openBuilder();
    const form = await select('health');
    const skillInput = within(form).getByPlaceholderText('Add a skill…');
    const addButton = within(form).getByRole('button', { name: 'Add' });

    // Add first skill
    await user.type(skillInput, 'deploy-helper');
    await user.click(addButton);

    expect(yamlData().spec.steps?.[0]?.skill).toBe('deploy-helper');
    expect(yamlData().spec.steps?.[0]).not.toHaveProperty('skills');
    expect(within(form).getByText('deploy-helper')).toBeInTheDocument();

    // Add second skill
    await user.type(skillInput, 'lint-check');
    await user.click(addButton);

    expect(yamlData().spec.steps?.[0]?.skills).toEqual(['deploy-helper', 'lint-check']);
    expect(yamlData().spec.steps?.[0]).not.toHaveProperty('skill');
    expect(within(form).getByText('lint-check')).toBeInTheDocument();

    // Remove first skill
    await user.click(within(form).getByRole('button', { name: 'Remove skill deploy-helper' }));
    expect(yamlData().spec.steps?.[0]?.skill).toBe('lint-check');
    expect(yamlData().spec.steps?.[0]).not.toHaveProperty('skills');

    // Remove remaining skill
    await user.click(within(form).getByRole('button', { name: 'Remove skill lint-check' }));
    expect(yamlData().spec.steps?.[0]).not.toHaveProperty('skill');
    expect(yamlData().spec.steps?.[0]).not.toHaveProperty('skills');

    await expectNoAxeViolations(document.querySelector('main')!);
  });

  it.each([
    [
      'approve',
      [
        'Prompt',
        'Approvals needed',
        'Agents may decide too',
        'When it times out',
        'Escalation timeout',
        'When it expires',
      ],
    ],
    ['canary', ['Continues when']],
    ['gate', ['Case 1: when', 'Default branch']],
    ['regions', ['Items', 'Each item is called', 'Key', 'At a time', 'Most items']],
    ['settle', ['Condition', 'At most', 'Pause between passes']],
    ['verify', ['Runbook', 'Version']],
    ['signal', ['Signal name', 'Give up after']],
    ['record', []],
    ['tidy', []],
  ])('shows %s only its kind’s fields', async (id, labels) => {
    await openBuilder();
    const form = await select(id as string);
    for (const label of labels as string[])
      expect(within(form).getByLabelText(label)).toBeInTheDocument();
    // No task fields on control steps.
    expect(within(form).queryByLabelText('Instructions')).not.toBeInTheDocument();
    expect(within(form).queryByLabelText('Assignee')).not.toBeInTheDocument();
  });

  it('edits containers, approvals, waits and calls', async () => {
    const { user } = await openBuilder();
    let form = await select('canary');
    await user.selectOptions(within(form).getByLabelText('Continues when'), 'any');
    expect(yamlText()).toContain('      title: Canary\n      join: any\n      parallel:');

    form = await select('gate');
    await user.click(within(form).getByRole('button', { name: 'Add a case' }));
    await user.click(within(form).getByRole('checkbox', { name: 'Default branch' }));
    const gate = yamlData().spec.steps[3] as { switch: { cases: unknown[]; default?: unknown } };
    expect(gate.switch.cases).toHaveLength(2);
    expect(gate.switch.default).toEqual({ steps: [] });

    form = await select('approve');
    await user.click(within(form).getByRole('button', { name: 'Add a person' }));
    await user.type(within(form).getByLabelText('Approving person 1'), 'Alice');
    expect(yamlText()).toContain('approvers: { teams: [oncall], users: [Alice] }');

    form = await select('signal');
    await user.click(within(form).getByRole('radio', { name: 'A duration' }));
    expect(yamlData().spec.steps[7]).toEqual({ id: 'signal', wait: { for: '1h' } });

    form = await select('verify');
    // The called runbook's inputs come from its published version.
    expect(within(form).getByRole('link', { name: /Post-deploy checks \(v3\)/ })).toBeVisible();
    await user.type(within(form).getByLabelText('Version'), '3');
    expect(yamlData().spec.steps[6]).toEqual({
      id: 'verify',
      call: { runbook: 'post-deploy-checks', version: 3 },
    });
  });
});

describe('the builder: steps', () => {
  it('inserts from the palette at the end of a list and focuses the new step', async () => {
    const { user } = await openBuilder();
    await user.click(screen.getByRole('button', { name: 'Add a step at the end of the runbook' }));
    // Radix names the menu by its trigger.
    const menu = await screen.findByRole('menu', { name: 'Add a step at the end of the runbook' });
    const kinds = within(menu)
      .getAllByRole('menuitem')
      .map((item) => item.textContent);
    expect(kinds).toHaveLength(13);
    expect(kinds[0]).toBe('TaskWork a person or an agent does, with instructions.');
    await user.click(within(menu).getByRole('menuitem', { name: /Approval/ }));
    await waitFor(() => expect(screen.getByRole('heading', { name: 'approval' })).toHaveFocus());
    const last = yamlData().spec.steps.at(-1);
    expect(last).toEqual({
      id: 'approval',
      approval: { prompt: 'Approve?', options: ['approve', 'reject'] },
    });
    expect(screen.getByTestId('builder-status')).toHaveTextContent('Added a approval step.');
  });

  it('names a new step from its title until its ID is edited', async () => {
    const { user } = await openBuilder();
    await user.click(screen.getByRole('button', { name: 'Add a step at the end of the runbook' }));
    await user.click(await screen.findByRole('menuitem', { name: /^Task/ }));
    const form = await stepPanel();
    // Focus moves to the new step's panel first; the person then edits its title.
    await waitFor(() =>
      expect(form.querySelector('[data-slot="node-details-heading"]')).toHaveFocus(),
    );
    const title = within(form).getByLabelText('Title');
    await user.type(title, 'Page on-call');
    expect(title).toHaveFocus();
    expect(yamlData().spec.steps.at(-1)).toEqual({ id: 'page_on_call', title: 'Page on-call' });
    // An ID that is taken is refused where it is typed.
    const id = within(form).getByLabelText('ID');
    await user.clear(id);
    await user.type(id, 'health{Enter}');
    expect(id).toHaveAccessibleDescription(
      /Another step \(Health answers 200\) already uses this ID/,
    );
    expect(yamlData().spec.steps.at(-1)).toMatchObject({ id: 'page_on_call' });
  });

  it('deletes, moves and duplicates from the step menu, with undo and redo', async () => {
    const { user } = await openBuilder();
    // Move "approve" up, from the menu.
    await user.click(within(card('approve')).getByRole('button', { name: 'Actions for approve' }));
    await user.click(await screen.findByRole('menuitem', { name: /Move up/ }));
    expect(
      yamlData()
        .spec.steps.map((s) => s.id)
        .slice(0, 2),
    ).toEqual(['approve', 'health']);
    // Into the parallel below it, then out again.
    await user.click(
      within(card('canary_eu')).getByRole('button', { name: 'Actions for Canary eu' }),
    );
    await user.click(await screen.findByRole('menuitem', { name: 'Move out of Canary' }));
    expect(
      yamlData()
        .spec.steps.map((s) => s.id)
        .slice(2, 4),
    ).toEqual(['canary', 'canary_eu']);
    // Duplicate.
    await user.click(within(card('signal')).getByRole('button', { name: 'Actions for signal' }));
    await user.click(await screen.findByRole('menuitem', { name: /Duplicate/ }));
    expect(yamlData().spec.steps.map((s) => s.id)).toContain('signal_copy');
    // Delete: focus moves to the next card.
    await user.click(
      within(card('signal_copy')).getByRole('button', { name: 'Actions for signal_copy' }),
    );
    await user.click(await screen.findByRole('menuitem', { name: /Delete/ }));
    expect(yamlData().spec.steps.map((s) => s.id)).not.toContain('signal_copy');
    await waitFor(() =>
      expect(card('record').querySelector('[data-slot="node-select"]')).toHaveFocus(),
    );
    expect(screen.getByTestId('builder-status')).toHaveTextContent('Deleted signal_copy.');
    // Undo brings it back; redo deletes it again.
    await user.click(screen.getByRole('button', { name: 'Undo' }));
    expect(yamlData().spec.steps.map((s) => s.id)).toContain('signal_copy');
    await user.click(screen.getByRole('button', { name: 'Redo' }));
    expect(yamlData().spec.steps.map((s) => s.id)).not.toContain('signal_copy');
  });

  it('moves and deletes the selected step from the keyboard', async () => {
    await openBuilder();
    await select('record');
    // Step shortcuts act on the focused card (or the canvas).
    const graph = card('record').querySelector<HTMLElement>('[data-slot="node-select"]')!;
    fireEvent.keyDown(graph, { key: 'ArrowUp', altKey: true });
    expect(
      yamlData()
        .spec.steps.map((s) => s.id)
        .slice(7, 9),
    ).toEqual(['record', 'signal']);
    fireEvent.keyDown(card('record').querySelector('[data-slot="node-select"]')!, {
      key: 'Delete',
    });
    expect(yamlData().spec.steps.map((s) => s.id)).not.toContain('record');
    fireEvent.keyDown(screen.getByTestId('builder'), { key: 'z', ctrlKey: true });
    expect(yamlData().spec.steps.map((s) => s.id)).toContain('record');
  });

  it('moves a step by dragging it onto a +', async () => {
    await openBuilder();
    const handle = card('record').querySelector('[data-slot="drag-handle"]')!;
    fireEvent.dragStart(handle, { dataTransfer: { setData: () => {}, effectAllowed: '' } });
    const target = await screen.findByRole('button', {
      name: 'Add a step at the end of tidy',
    });
    fireEvent.dragOver(target, { dataTransfer: { dropEffect: '' } });
    fireEvent.drop(target, { dataTransfer: {} });
    const tidy = yamlData().spec.steps.find((s) => s.id === 'tidy') as { steps: { id: string }[] };
    expect(tidy.steps.map((s) => s.id)).toEqual(['inner', 'record']);
  });
});

describe('the builder: validation, sync and read-only', () => {
  const approvers: Diagnostic = {
    path: '/spec/steps/1/approval/approvers',
    code: 'spec.approvers_required',
    message: 'Approval must specify at least one approver.',
    line: 15,
    column: 9,
  };

  it('shows problems on the card, under the field and in the problems list', async () => {
    const { user } = await openBuilder({}, [approvers]);
    await waitFor(() => expect(card('approve')).toHaveAttribute('data-issues', '1'));
    expect(card('approve').querySelector('[data-slot="node-select"]')).toHaveAccessibleName(
      /1 problem/,
    );
    const problems = within(screen.getByTestId('diagnostics')).getByRole('list', {
      name: 'Problems',
    });
    await user.click(within(problems).getByRole('button', { name: 'Show approve' }));
    const form = await stepPanel();
    expect(within(form).getByRole('heading', { name: 'approve' })).toBeInTheDocument();
    expect(within(form).getByLabelText('Approving team 1')).toHaveAttribute('aria-invalid', 'true');
    expect(form).toHaveTextContent('Approval must specify at least one approver.');
    expect(screen.getByTestId('builder-summary')).toHaveTextContent('at most 7 step runs');
  });

  it('shows the server’s problems after a save, replacing the browser’s', async () => {
    const serverProblem: Diagnostic = {
      path: '/spec/steps/6/call/runbook',
      code: 'spec.call_unresolved',
      message: 'post-deploy-checks is not published.',
      line: 41,
      column: 18,
    };
    server.use(
      http.patch('*/v1/runbooks/:rb/draft', async ({ request }) => {
        const body = (await request.json()) as { source: string };
        return HttpResponse.json(
          { runbook_id: RB, source: body.source, revision: 5, diagnostics: [serverProblem] },
          { headers: { ETag: etagOf(5) } },
        );
      }),
    );
    const { user } = await openBuilder({}, [approvers]);
    const form = await select('health');
    await user.type(within(form).getByLabelText('Timeout'), '5m');
    await user.click(screen.getByRole('button', { name: 'Save draft' }));
    await screen.findByText(/Saved revision 5/);
    await waitFor(() => expect(card('verify')).toHaveAttribute('data-issues', '1'));
    expect(card('approve')).not.toHaveAttribute('data-issues');
  });

  it('follows edits typed in the YAML tab', async () => {
    await openBuilder();
    fireEvent.change(yamlBox(), {
      target: {
        value: SOURCE.replace('    - id: tidy\n', '    - id: tidy\n      title: Tidy up\n'),
      },
    });
    await waitFor(() => expect(card('tidy')).toHaveTextContent('Tidy up'));
  });

  it('shows the last good graph read-only while the YAML does not parse', async () => {
    const { user } = await openBuilder();
    fireEvent.change(yamlBox(), { target: { value: `${SOURCE}  bad: [\n` } });
    const note = await screen.findByTestId('builder-parse-error');
    expect(note).toHaveTextContent('The YAML does not parse, so the builder is read-only.');
    expect(card('health')).toBeInTheDocument();
    expect(
      document.querySelector('[data-slot="insert-button"], [data-slot="slot-button"]'),
    ).toBeNull();
    expect(document.querySelector('[data-slot="step-menu"]')).toBeNull();
    await user.click(within(note).getByRole('button', { name: /Go to line \d+ in the YAML/ }));
    expect(screen.getByRole('tab', { name: 'YAML' })).toHaveAttribute('aria-selected', 'true');
  });

  it('keeps a Git-managed runbook read-only and says why', async () => {
    await openBuilder({ origin: 'git' });
    expect(screen.getByTestId('read-only')).toHaveTextContent(
      'managed in Git. Change it through a pull request',
    );
    expect(document.querySelector('[data-slot="step-menu"]')).toBeNull();
    expect(screen.queryByRole('toolbar', { name: 'Builder' })).not.toBeInTheDocument();
    fireEvent.click(card('health').querySelector('[data-slot="node-select"]')!);
    // The read-only panel describes the step; it does not edit it.
    await waitFor(() =>
      expect(screen.getByRole('heading', { name: 'Health answers 200' })).toBeInTheDocument(),
    );
    expect(screen.queryByLabelText('Instructions')).not.toBeInTheDocument();
  });

  it('reloads after a conflict and reapplies the builder’s changes on top', async () => {
    const shared = state();
    server.use(
      ...editorHandlers(shared),
      http.patch('*/v1/runbooks/:rb/draft', () => problem(412, 'catalog.draft_conflict')),
    );
    renderEditor(fakeCompiler(() => []));
    await waitFor(() => card('health'));
    const user = userEvent.setup();
    const form = await select('signal');
    await user.type(within(form).getByLabelText('Give up after'), '2h');
    // Someone else changes the description meanwhile.
    shared.draft = {
      source: SOURCE.replace(
        '  title: Release checks\n',
        '  title: Release checks\n  description: Theirs\n',
      ),
      revision: 5,
    };
    await user.click(screen.getByRole('button', { name: 'Save draft' }));
    const conflict = await screen.findByTestId('conflict');
    await user.click(
      within(conflict).getByRole('button', { name: 'Reload and reapply my changes' }),
    );
    expect(await screen.findByTestId('editor-notice')).toHaveTextContent(
      'Loaded revision 5 and reapplied your changes on top. Review them, then save.',
    );
    expect(yamlText()).toContain('  description: Theirs\n');
    expect(yamlText()).toContain('wait: { signal: go, timeout: 2h }');
    // Unsaved: the merged text is the author's to review and save.
    expect(screen.getByText('Unsaved changes.')).toBeInTheDocument();
  });
});

describe('the builder: review round 1', () => {
  async function openWith(
    source: string,
    wait: () => unknown = () => screen.getByTestId('builder'),
  ) {
    server.use(...editorHandlers(state({ draft: { source, revision: 4 } })));
    const utils = renderEditor(fakeCompiler(() => []));
    await waitFor(wait);
    return { user: userEvent.setup(), ...utils };
  }
  // The toolbar's "Add step" (an empty runbook has no other end-of-list button).
  const addTask = async (user: ReturnType<typeof userEvent.setup>) => {
    const toolbar = screen.getByRole('toolbar', { name: 'Builder' });
    await user.click(within(toolbar).getByRole('button', { name: /Add step/ }));
    await user.click(await screen.findByRole('menuitem', { name: /^Task/ }));
  };

  it.each([
    ['spec:', 'metadata:\n  id: empty\nspec:\n'],
    ['spec: null', 'metadata:\n  id: empty\nspec: null\n'],
    ['no spec', 'metadata:\n  id: empty\n'],
  ])('adds a step under %s', async (_, source) => {
    const { user } = await openWith(source);
    await addTask(user);
    await waitFor(() => expect(yamlData().spec.steps).toEqual([{ id: 'task' }]));
    expect(screen.queryByTestId('builder-error')).not.toBeInTheDocument();
  });

  it('sets the title under an empty metadata', async () => {
    const { user } = await openWith('metadata:\nspec:\n  steps: []\n');
    const settings = document.querySelector<HTMLElement>('[data-slot="settings-panel"]')!;
    await user.type(within(settings).getByLabelText('Title'), 'T');
    expect(parseDocument(yamlText()).toJS()).toMatchObject({ metadata: { title: 'T' } });
  });

  it('shows anchors, aliases and tags read-only and says why', async () => {
    await openWith(
      'metadata:\n  id: anchors\nspec:\n  defaults: &d\n    actor: human\n  steps:\n    - id: a\n',
      () => card('a'),
    );
    expect(screen.getByTestId('builder-locked')).toHaveTextContent(/anchors, aliases or tags/);
    expect(screen.queryByRole('toolbar', { name: 'Builder' })).not.toBeInTheDocument();
    expect(
      within(card('a')).queryByRole('button', { name: /Actions for/ }),
    ).not.toBeInTheDocument();
  });

  it('shows an edit that fails as an error and leaves the YAML as it was', async () => {
    const { user } = await openWith('hello\n');
    await addTask(user);
    expect(await screen.findByTestId('builder-error')).toHaveTextContent(
      /could not be made, so the YAML is unchanged/,
    );
    expect(yamlText()).toBe('hello\n');
  });

  // UI-4b item 3.
  it('says when adding a step replaced a value that was not a list', async () => {
    const { user } = await openWith('metadata:\n  id: odd\nspec:\n  steps: 5 # five\n');
    await addTask(user);
    await waitFor(() => expect(yamlData().spec.steps).toEqual([{ id: 'task' }]));
    expect(yamlText()).toContain('  steps: # five\n');
    expect(screen.getByTestId('builder-replaced')).toHaveTextContent(
      'spec.steps was 5, not a list, so it was replaced. Undo (Ctrl+Z) brings it back.',
    );
    expect(screen.queryByTestId('builder-normalized')).not.toBeInTheDocument();
  });

  it('warns when an edit had to rewrite the whole YAML', async () => {
    const { user } = await openWith('# only a comment\n');
    await addTask(user);
    await waitFor(() => expect(yamlData().spec.steps).toEqual([{ id: 'task' }]));
    expect(screen.getByTestId('builder-normalized')).toHaveTextContent(/standard layout/);
  });

  it('edits one checklist item in place, keeping comments and items it does not show', async () => {
    const source = SOURCE.replace(
      '      x-note: keep me\n',
      '      x-note: keep me\n      checklist:\n        - { note: kept }\n        - one # c1\n        # between\n        - two\n',
    );
    const { user } = await openWith(source, () => card('health'));
    const panel = await select('health');
    const second = within(panel).getByLabelText('Checklist item 2');
    await user.type(second, 's');
    expect(yamlText()).toContain(
      '        - { note: kept }\n        - one # c1\n        # between\n        - twos\n',
    );
    await user.click(within(panel).getByRole('button', { name: 'Remove checklist item 1' }));
    expect(yamlText()).toContain('        - { note: kept }\n        # between\n        - twos\n');
  });

  it('says the same thing twice when the same thing happens twice', async () => {
    const { user } = await openBuilder();
    const status = screen.getByTestId('builder-status');
    const seen: string[] = [];
    const observer = new MutationObserver(() => seen.push(status.textContent ?? ''));
    observer.observe(status, { childList: true, characterData: true, subtree: true });
    await addTask(user);
    await waitFor(() => expect(status).toHaveTextContent('Added a task step.'));
    seen.length = 0;
    await addTask(user);
    await waitFor(() => expect(seen).toContain(''));
    await waitFor(() => expect(status).toHaveTextContent('Added a task step.'));
    observer.disconnect();
  });

  it('deletes from the keyboard only with a card focused, and asks before deleting a container’s steps', async () => {
    await openBuilder();
    const panel = await select('record');
    // Focus is on the panel's heading: Backspace there deletes nothing.
    fireEvent.keyDown(panel.querySelector('[data-slot="node-details-heading"]')!, {
      key: 'Backspace',
    });
    fireEvent.keyDown(within(panel).getAllByRole('button')[0]!, { key: 'Delete' });
    expect(yamlData().spec.steps.map((s) => s.id)).toContain('record');
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    try {
      await select('canary');
      fireEvent.keyDown(card('canary').querySelector('[data-slot="node-select"]')!, {
        key: 'Delete',
      });
      expect(confirm).toHaveBeenCalledWith(expect.stringMatching(/Canary and the 2 steps inside/));
      expect(yamlData().spec.steps.map((s) => s.id)).toContain('canary');
      confirm.mockReturnValue(true);
      fireEvent.keyDown(card('canary').querySelector('[data-slot="node-select"]')!, {
        key: 'Delete',
      });
      expect(yamlData().spec.steps.map((s) => s.id)).not.toContain('canary');
    } finally {
      confirm.mockRestore();
    }
  });
});

describe('the builder graph’s insert points', () => {
  it('puts a + on every edge into a step and a slot at the end of every list', () => {
    const read = readBuilder(SOURCE);
    if (!read.ok) throw new Error('no model');
    const { edges, slots } = layout(read.model.outline, { builder: true });
    const inserts = edges.filter((e) => e.data?.showInsert).map((e) => e.data?.insert);
    expect(inserts).toEqual(
      expect.arrayContaining([
        { body: '/spec/steps', index: 1 },
        { body: '/spec/steps/2/parallel', index: 0 },
        { body: '/spec/steps/2/parallel', index: 1 },
        { body: '/spec/steps/3/switch/cases/0/steps', index: 0 },
        { body: '/spec/steps/4/foreach/steps', index: 0 },
        { body: '/spec/steps/5/loop/steps', index: 0 },
        { body: '/spec/steps/9/steps', index: 0 },
      ]),
    );
    expect(slots.map((s) => [s.data.slot.body, s.data.slot.index])).toEqual(
      expect.arrayContaining([
        ['/spec/steps', 10],
        ['/spec/steps/2/parallel', 2],
        ['/spec/steps/3/switch/cases/0/steps', 1],
        ['/spec/steps/3/switch/default/steps', 0],
        ['/spec/steps/4/foreach/steps', 1],
        ['/spec/steps/5/loop/steps', 1],
        ['/spec/steps/9/steps', 1],
      ]),
    );
    // Run views draw no slots and no insert points.
    const plain = layout(read.model.outline);
    expect(plain.slots).toEqual([]);
    expect(plain.edges.some((e) => e.data?.insert)).toBe(false);
  });

  it('is axe-clean with a step selected', async () => {
    await openBuilder();
    await select('approve');
    await act(async () => {
      await expectNoAxeViolations(document.querySelector('main')!);
    });
  });
});
