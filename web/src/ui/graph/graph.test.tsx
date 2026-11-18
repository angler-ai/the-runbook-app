import { describe, it, expect, vi } from 'vitest';
import { useState } from 'react';
import { render, screen, within, fireEvent, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expectNoAxeViolations } from '../../test/axe';
import { RunbookGraph, GRAPH_FIT_VIEW_OPTIONS, findNode, rememberGraphView } from './RunbookGraph';
import { layout, countNodes, containerIds, NODE_HEIGHT, NODE_WIDTH } from './layout';
import { TemplateText, templateParts, plainTemplateText } from './TemplateText';
import { topAlignedViewport, nodeBounds, READABLE_MIN_ZOOM } from './viewport';
import { branchLabel, joinLabel, nodeTitle, waitLabel, shorten } from './labels';
import type { Outline, OutlineNode } from '../types';
import postDeployJson from '../outline/fixtures/post-deploy.outline.json';
import releaseTrainJson from './fixtures/release-train.outline.json';

const postDeploy = postDeployJson as Outline;
const releaseTrain = releaseTrainJson as Outline;

// React Flow keeps nodes `visibility: hidden` until it measures them, which
// jsdom never does: query cards by their data hooks, not by role.
function toggleOf(container: HTMLElement, id: string): HTMLElement {
  return graphNode(container, id).querySelector<HTMLElement>('[data-slot="collapse-toggle"]')!;
}

function graphNode(container: HTMLElement, id: string): HTMLElement {
  const el = container.querySelector<HTMLElement>(`[data-node-id="${id}"]`);
  if (!el) throw new Error(`no graph node ${id}`);
  return el;
}

describe('layout determinism and structure', () => {
  const outline = postDeploy;

  it('produces byte-for-byte identical output on repeated runs', () => {
    const run1 = layout(outline);
    const run2 = layout(outline);

    expect(run1).toEqual(run2);
    expect(JSON.stringify(run1)).toBe(JSON.stringify(run2));
  });

  it('converts every OutlineNode to a graph node with correct data', () => {
    const { nodes, edges } = layout(outline);

    // 13 outline nodes in post-deploy:
    // smoke (parallel) -> [api_health, login_flow]
    // regions (foreach) -> [error_rate]
    // stabilize (loop) -> [recheck]
    // decide (switch) -> branch 1 [signoff, rollback, mark_major], branch 2 [mark_healthy]
    // finally -> [notify]
    expect(nodes).toHaveLength(13);
    expect(nodes.map((n) => n.id).sort()).toEqual(
      [
        'smoke',
        'api_health',
        'login_flow',
        'regions',
        'error_rate',
        'stabilize',
        'recheck',
        'decide',
        'signoff',
        'rollback',
        'mark_major',
        'mark_healthy',
        'notify',
      ].sort(),
    );

    expect(nodes.find((n) => n.id === 'rollback')?.data.node.destructive).toBe(true);
    expect(nodes.find((n) => n.id === 'notify')?.data.isFinally).toBe(true);

    // Parallel fan-out and join into regions
    expect(edges.find((e) => e.source === 'smoke' && e.target === 'api_health')).toBeDefined();
    expect(edges.find((e) => e.source === 'smoke' && e.target === 'login_flow')).toBeDefined();
    expect(edges.find((e) => e.source === 'api_health' && e.target === 'regions')).toBeDefined();
    expect(edges.find((e) => e.source === 'login_flow' && e.target === 'regions')).toBeDefined();

    // Loop back-edge, drawn dashed
    const loopBack = edges.find((e) => e.source === 'recheck' && e.target === 'stabilize');
    expect(loopBack?.label).toBe('repeat');
    expect(loopBack?.data?.variant).toBe('repeat');

    // Decision branches are labelled by their condition (shortened on the edge)
    const decideToFailed = edges.find((e) => e.source === 'decide' && e.target === 'signoff');
    expect(decideToFailed?.data?.variant).toBe('case');
    expect(String(decideToFailed?.label)).toMatch(/^steps\.smoke\.verdict == 'fail'.*…$/);
    expect(decideToFailed?.data?.fullLabel).toContain("steps.stabilize.verdict == 'fail'");
    expect(edges.find((e) => e.source === 'decide' && e.target === 'mark_healthy')?.label).toBe(
      'default',
    );

    // Finally join edge
    const finallyEdges = edges.filter((e) => e.target === 'notify');
    expect(finallyEdges.length).toBeGreaterThan(0);
    expect(finallyEdges[0]?.label).toBe('finally');
    expect(finallyEdges[0]?.data?.variant).toBe('finally');
  });

  it('accurately counts nodes', () => {
    expect(countNodes(outline)).toBe(13);
    expect(countNodes(releaseTrain)).toBe(27);
  });
});

describe('release-train layout', () => {
  const { nodes, edges } = layout(releaseTrain);
  const edge = (source: string, target: string) =>
    edges.find((e) => e.source === source && e.target === target);

  it('draws catch as a dashed "on failure" lane from its group', () => {
    const toCatch = edge('preflight', 'preflight_failed');
    expect(toCatch?.label).toBe('on failure');
    expect(toCatch?.data?.variant).toBe('catch');
    expect(toCatch?.style?.strokeDasharray).toBeDefined();
    expect(nodes.find((n) => n.id === 'preflight_failed')?.data.lane).toBe('catch');
  });

  it('stops flow after an unconditional end but not a guarded one', () => {
    // stop_after_preflight ends the run: nothing follows it.
    expect(edges.filter((e) => e.source === 'stop_after_preflight')).toHaveLength(0);
    // hold_if_rejected has an `if`: when it does not end, flow continues.
    expect(edge('hold_if_rejected', 'canary')).toBeDefined();
  });

  it('labels switch cases by their condition and parallels by their join', () => {
    expect(edge('risk_gate', 'cab_review')?.label).toBe("inputs.risk == 'high'");
    expect(edge('risk_gate', 'lead_signoff')?.label).toBe("inputs.risk == 'medium'");
    expect(edge('risk_gate', 'auto_ok')?.label).toBe('default');
    const canary = findNode(releaseTrain, 'canary');
    expect(canary?.join && joinLabel(canary.join)).toBe('Join: any');
  });

  it('continues after a loop from its body and repeats back to its header', () => {
    expect(edge('region_health', 'settle')?.data?.variant).toBe('repeat');
    expect(edge('region_health', 'verify')).toBeDefined();
  });

  it('runs the runbook finally after every open path', () => {
    expect(edge('shipped', 'retro_notes')?.label).toBe('finally');
  });

  it('collapses a container: descendants hidden, flow continues from it', () => {
    const collapsed = layout(releaseTrain, { collapsed: new Set(['rollout']) });
    const ids = collapsed.nodes.map((n) => n.id);
    expect(ids).toContain('rollout');
    expect(ids).not.toContain('deploy_region');
    expect(ids).not.toContain('region_health');
    const rollout = collapsed.nodes.find((n) => n.id === 'rollout');
    expect(rollout?.data.collapsed).toBe(true);
    expect(rollout?.data.hiddenCount).toBe(3);
    expect(
      collapsed.edges.find((e) => e.source === 'rollout' && e.target === 'verify'),
    ).toBeDefined();
  });

  it('lists every container', () => {
    expect(containerIds(releaseTrain)).toEqual([
      'preflight',
      'checks',
      'risk_gate',
      'canary',
      'rollout',
      'settle',
      'announce',
    ]);
  });
});

describe('fit from the top', () => {
  it('fits a narrow graph at 100%, centred, with its first step at the top', () => {
    const v = topAlignedViewport(
      { x: 0, y: 0, width: 400, height: 3000 },
      { width: 1000, height: 600 },
      { padding: 20 },
    );
    expect(v).toEqual({ x: 300, y: 20, zoom: 1 });
  });

  it('shrinks a wide graph to the pane width but never below a readable zoom', () => {
    const fits = topAlignedViewport(
      { x: 0, y: 0, width: 1000, height: 3000 },
      { width: 940, height: 600 },
      { padding: 20 },
    );
    expect(fits.zoom).toBeCloseTo(0.9);
    expect(fits.y).toBe(20);

    const huge = topAlignedViewport(
      { x: 0, y: 0, width: 4000, height: 3000 },
      { width: 800, height: 600 },
      { padding: 20 },
    );
    expect(huge.zoom).toBe(READABLE_MIN_ZOOM);
    // Too wide even then: start at its left edge.
    expect(huge.x).toBe(20);
    expect(huge.y).toBe(20);
  });

  it('centres a too-wide graph on its first step, within the graph', () => {
    const bounds = { x: 0, y: 0, width: 4000, height: 3000 };
    const pane = { width: 800, height: 600 };
    const v = topAlignedViewport(bounds, pane, { padding: 20, anchorX: 2000 });
    expect(v.zoom).toBe(READABLE_MIN_ZOOM);
    expect(v.x).toBe(Math.round(400 - 2000 * READABLE_MIN_ZOOM));
    // An anchor near an edge never scrolls past that edge.
    expect(topAlignedViewport(bounds, pane, { padding: 20, anchorX: 10 }).x).toBe(20);
    expect(topAlignedViewport(bounds, pane, { padding: 20, anchorX: 3990 }).x).toBe(
      Math.round(800 - 20 - 4000 * READABLE_MIN_ZOOM),
    );
  });

  it('accounts for graphs that do not start at the origin', () => {
    const v = topAlignedViewport(
      { x: -100, y: 50, width: 200, height: 100 },
      { width: 400, height: 400 },
      { padding: 10 },
    );
    expect(v).toEqual({ x: 200, y: -40, zoom: 1 });
  });

  it('measures the box around laid-out nodes', () => {
    const { nodes } = layout(releaseTrain);
    const bounds = nodeBounds(nodes, { width: NODE_WIDTH, height: NODE_HEIGHT });
    const top = Math.min(...nodes.map((n) => n.position.y));
    expect(bounds.y).toBe(top);
    expect(nodes.find((n) => n.position.y === top)?.id).toBe('preflight');
    expect(nodeBounds([], { width: 1, height: 1 })).toEqual({ x: 0, y: 0, width: 0, height: 0 });
  });
});

describe('labels', () => {
  it('names joins', () => {
    expect(joinLabel({ kind: 'all' })).toBe('Join: all');
    expect(joinLabel({ kind: 'any' })).toBe('Join: any');
    expect(joinLabel({ kind: 'at_least', count: 2 })).toBe('Join: at least 2');
  });

  it('names waits, signals first-class', () => {
    expect(waitLabel({ kind: 'signal', signal: 'release_email_sent' })).toEqual({
      prefix: 'Waits for signal',
      value: 'release_email_sent',
    });
    expect(waitLabel({ kind: 'for', duration: '1h30m' })).toEqual({
      prefix: 'Waits',
      value: '1h30m',
    });
  });

  it('names branches by role or condition', () => {
    expect(branchLabel({ label: 'x == 1', kind: 'case', children: [] })).toBe('x == 1');
    expect(branchLabel({ label: 'catch', kind: 'catch', children: [] })).toBe('on failure');
    expect(branchLabel({ label: 'default', kind: 'default', children: [] })).toBe('default');
    // Outlines from before UI-2 have no kind.
    expect(branchLabel({ label: 'case 1', children: [] })).toBe('case 1');
    expect(shorten('a'.repeat(40), 10)).toBe(`${'a'.repeat(9)}…`);
  });

  it('titles untitled set and end steps', () => {
    expect(nodeTitle(findNode(releaseTrain, 'shipped')!)).toBe('Set outcome, decision');
    expect(nodeTitle(findNode(releaseTrain, 'stop_after_preflight')!)).toBe('End: failed');
  });
});

describe('TemplateText', () => {
  it('splits text and templates in order', () => {
    expect(templateParts('Deploy to ${{ region }} now')).toEqual([
      { kind: 'text', text: 'Deploy to ' },
      { kind: 'expr', text: 'region' },
      { kind: 'text', text: ' now' },
    ]);
    expect(templateParts('${{a}}${{ b.c }}')).toEqual([
      { kind: 'expr', text: 'a' },
      { kind: 'expr', text: 'b.c' },
    ]);
    expect(templateParts('no templates')).toEqual([{ kind: 'text', text: 'no templates' }]);
    expect(plainTemplateText('Roll back ${{ inputs.service }}')).toBe('Roll back inputs.service');
  });

  it('renders each template as a monospace chip', () => {
    const { container } = render(<TemplateText text="Error rate in ${{ region }}" />);
    const chips = container.querySelectorAll('[data-slot="template-chip"]');
    expect(chips).toHaveLength(1);
    expect(chips[0]?.tagName).toBe('CODE');
    expect(chips[0]).toHaveTextContent(/^region$/);
    expect(container).toHaveTextContent('Error rate in region');
    expect(container.textContent).not.toContain('${{');
  });
});

// Card clicks use fireEvent: user-event's mousedown carries no `view`, which
// d3-zoom reads in jsdom. Keyboard paths use user-event.
describe('RunbookGraph component', () => {
  it('renders graph nodes, step budget badge, and is axe-clean', async () => {
    const { container } = render(
      <div style={{ width: 800, height: 600 }}>
        <RunbookGraph outline={postDeploy} />
      </div>,
    );

    expect(container.querySelector('[data-slot="runbook-graph"]')).toBeInTheDocument();
    expect(screen.getByText('Step budget:')).toBeInTheDocument();
    expect(screen.getByText('20 max')).toBeInTheDocument();

    expect(screen.getByText('Smoke tests')).toBeInTheDocument();
    expect(screen.getByText('Health endpoint returns 200')).toBeInTheDocument();
    // Templates in titles are chips, not raw text.
    const rollback = graphNode(container, 'rollback');
    expect(rollback).toHaveTextContent('Roll back inputs.service');
    expect(within(rollback).getByText('inputs.service').tagName).toBe('CODE');
    expect(screen.getByText('Post the report to the deploys channel')).toBeInTheDocument();

    expect(within(rollback).getByText('destructive')).toBeInTheDocument();
    expect(
      graphNode(container, 'notify').querySelector('[data-slot="finally-badge"]'),
    ).toHaveTextContent('finally');

    await expectNoAxeViolations(container);
  });

  it('caps at 500 nodes and renders EmptyState without running layout', () => {
    const largeSteps: OutlineNode[] = [];
    for (let i = 1; i <= 501; i++) {
      largeSteps.push({ id: `step_${i}`, title: `Step ${i}`, path: `step_${i}`, kind: 'task' });
    }
    const largeOutline: Outline = {
      id: 'huge',
      title: 'Huge Runbook',
      stepBudget: 1000,
      steps: largeSteps,
      finally: [],
    };

    const { container } = render(<RunbookGraph outline={largeOutline} />);

    expect(screen.getByText('Too large to draw — use the outline')).toBeInTheDocument();
    expect(
      screen.getByText('Use the outline view instead for large runbooks.'),
    ).toBeInTheDocument();
    expect(container.querySelector('[data-slot="runbook-graph"]')).toBeNull();
  });

  it('configures the fit-all control with a minimum zoom and padding', () => {
    expect(GRAPH_FIT_VIEW_OPTIONS.minZoom).toBeGreaterThanOrEqual(0.6);
    expect(Number(GRAPH_FIT_VIEW_OPTIONS.padding)).toBeGreaterThan(0);
  });

  it('labels kinds on the cards: join, signal, actor, call link', () => {
    const { container } = render(
      <RunbookGraph outline={releaseTrain} callHref={(slug) => `/p/demo/runbooks/${slug}`} />,
    );
    expect(graphNode(container, 'canary')).toHaveTextContent('Join: any');
    expect(graphNode(container, 'checks')).toHaveTextContent('Join: all');
    const customer = graphNode(container, 'customer_go');
    expect(customer).toHaveTextContent('Waits for signal release_email_sent');
    expect(
      graphNode(container, 'status_page').querySelector('[data-slot="actor-badge"]'),
    ).toHaveTextContent('Human');
    expect(
      graphNode(container, 'build').querySelector('[data-slot="actor-badge"]'),
    ).toHaveTextContent('Agent');
    expect(graphNode(container, 'risk_gate')).toHaveTextContent('2 cases + default');
    const link = graphNode(container, 'verify').querySelector('[data-slot="call-link"]');
    expect(link).toHaveAttribute('href', '/p/demo/runbooks/post-deploy-checks');
    expect(graphNode(container, 'deploy_region')).toHaveTextContent('destructive');
    expect(graphNode(container, 'deploy_region')).toHaveTextContent('Deploy to region');
  });

  it('offers the minimap only for large graphs', () => {
    const { unmount } = render(<RunbookGraph outline={releaseTrain} />);
    expect(screen.getByRole('button', { name: /overview map/ })).toBeInTheDocument();
    unmount();
    render(
      <RunbookGraph
        outline={{
          id: 'tiny',
          title: 'Tiny',
          stepBudget: 1,
          steps: [{ id: 'a', title: 'A', path: '/spec/steps/0', kind: 'task' }],
          finally: [],
        }}
      />,
    );
    expect(screen.queryByLabelText('Overview of the whole graph')).toBeNull();
    expect(screen.queryByRole('button', { name: /overview map/ })).toBeNull();
  });

  it('collapses and expands a container with its toggle', async () => {
    const { container } = render(<RunbookGraph outline={releaseTrain} />);
    expect(container.querySelector('[data-node-id="deploy_region"]')).not.toBeNull();

    const toggle = toggleOf(container, 'rollout');
    expect(toggle).toHaveAttribute('aria-label', 'Collapse rollout');
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    fireEvent.click(toggle);
    expect(container.querySelector('[data-node-id="deploy_region"]')).toBeNull();
    const expand = toggleOf(container, 'rollout');
    expect(expand).toHaveAttribute('aria-label', 'Expand rollout');
    expect(expand).toHaveAttribute('aria-expanded', 'false');
    expect(expand).toHaveTextContent('3 hidden');

    fireEvent.click(expand);
    expect(container.querySelector('[data-node-id="deploy_region"]')).not.toBeNull();
  });

  it('starts collapsed from initialCollapsed', () => {
    const { container } = render(
      <RunbookGraph outline={releaseTrain} initialCollapsed={['preflight']} />,
    );
    expect(container.querySelector('[data-node-id="build"]')).toBeNull();
    expect(toggleOf(container, 'preflight')).toHaveAttribute('aria-label', 'Expand preflight');
  });

  it('selects with the keyboard and shows the step in the side panel; Esc closes it', async () => {
    const user = userEvent.setup();
    const { container } = render(<RunbookGraph outline={releaseTrain} />);
    expect(screen.queryByRole('complementary', { name: 'Step details' })).toBeNull();

    // Each card is one tab stop (its title button) in tree order, so Tab
    // walks the steps top to bottom; containers add their collapse toggle.
    const stops = [...container.querySelectorAll<HTMLElement>('button, a[href]')]
      .filter((el) => el.closest('[data-node-id]'))
      .map((el) =>
        el.dataset.slot === 'node-select'
          ? el.closest('[data-node-id]')!.getAttribute('data-node-id')
          : `${el.closest('[data-node-id]')!.getAttribute('data-node-id')}:${el.dataset.slot}`,
      );
    expect(stops.slice(0, 7)).toEqual([
      'preflight',
      'preflight:collapse-toggle',
      'checks',
      'checks:collapse-toggle',
      'changelog',
      'migrations',
      'freeze',
    ]);
    // React Flow's own node wrappers are not extra tab stops.
    expect(container.querySelector('.react-flow__node[tabindex="0"]')).toBeNull();
    const migrations = graphNode(container, 'migrations').querySelector<HTMLElement>(
      '[data-slot="node-select"]',
    )!;
    migrations.focus();
    await user.keyboard('{Enter}');

    const panel = screen.getByRole('complementary', { name: 'Step details' });
    expect(within(panel).getByRole('heading', { level: 2 })).toHaveTextContent(
      'Database migrations are backward compatible',
    );
    expect(panel).toHaveTextContent('migrations');
    expect(panel).toHaveTextContent('/spec/steps/0/steps/0/parallel/1');
    expect(panel).toHaveTextContent('Agent');
    expect(panel).toHaveTextContent('20m');
    expect(panel).toHaveTextContent('Dry-run pending migrations against a replica');
    expect(panel).toHaveTextContent('pending integer, required');
    expect(migrations).toHaveAttribute('aria-pressed', 'true');

    await user.keyboard('{Escape}');
    expect(screen.queryByRole('complementary', { name: 'Step details' })).toBeNull();
  });

  it('selects with Space and closes with the panel button', async () => {
    const user = userEvent.setup();
    const { container } = render(<RunbookGraph outline={releaseTrain} />);
    graphNode(container, 'customer_go')
      .querySelector<HTMLElement>('[data-slot="node-select"]')!
      .focus();
    await user.keyboard(' ');
    const panel = screen.getByRole('complementary', { name: 'Step details' });
    expect(panel).toHaveTextContent('for signal release_email_sent');
    expect(panel).toHaveTextContent('1d');
    await user.click(within(panel).getByRole('button', { name: 'Close step details' }));
    expect(screen.queryByRole('complementary', { name: 'Step details' })).toBeNull();
  });

  it('shows kind-specific facts: approval, call, switch, join', async () => {
    const { container } = render(
      <RunbookGraph outline={releaseTrain} callHref={(slug) => `/p/demo/runbooks/${slug}`} />,
    );
    const open = async (id: string) => {
      fireEvent.click(
        graphNode(container, id).querySelector<HTMLElement>('[data-slot="node-select"]')!,
      );
      return screen.getByRole('complementary', { name: 'Step details' });
    };

    const cab = await open('cab_review');
    expect(cab).toHaveTextContent('Approve the high-risk release inputs.version?');
    expect(cab).toHaveTextContent('ship / hold');
    expect(cab).toHaveTextContent('team:oncall');
    expect(cab).toHaveTextContent('4h, then escalates to team:customer-success for 1h');

    const verify = await open('verify');
    expect(within(verify).getByRole('link', { name: /post-deploy-checks/ })).toHaveAttribute(
      'href',
      '/p/demo/runbooks/post-deploy-checks',
    );
    expect(verify).toHaveTextContent('regions, service, version');

    const gate = await open('risk_gate');
    expect(gate).toHaveTextContent("inputs.risk == 'high'");
    expect(gate).toHaveTextContent("inputs.risk == 'medium'");
    expect(gate).toHaveTextContent('default → auto_ok');

    const canary = await open('canary');
    expect(canary).toHaveTextContent('Continues when the first branch succeeds');

    const deploy = await open('deploy_region');
    expect(deploy).toHaveTextContent('Destructive');

    await expectNoAxeViolations(container);
  });

  it('supports controlled selection', async () => {
    function Host() {
      const [selected, setSelected] = useState<string | undefined>('build');
      return (
        <>
          <output data-testid="selected">{selected ?? 'none'}</output>
          <RunbookGraph outline={releaseTrain} selectedId={selected} onSelect={setSelected} />
        </>
      );
    }
    const { container } = render(<Host />);
    expect(screen.getByRole('complementary', { name: 'Step details' })).toHaveTextContent(
      'Build and sign the release artifacts',
    );
    fireEvent.click(
      graphNode(container, 'verify').querySelector<HTMLElement>('[data-slot="node-select"]')!,
    );
    expect(screen.getByTestId('selected')).toHaveTextContent('verify');
    fireEvent.keyDown(screen.getByRole('complementary', { name: 'Step details' }), {
      key: 'Escape',
    });
    expect(screen.getByTestId('selected')).toHaveTextContent('none');
  });
});

describe('the minimap', () => {
  it('can size every node (they are controlled, so never measured back)', async () => {
    const { layout: run } = await import('./layout');
    const outline = (await import('./fixtures/release-train.outline.json'))
      .default as unknown as Parameters<typeof run>[0];
    const { nodes } = run(outline);
    expect(nodes.length).toBeGreaterThan(12);
    for (const node of nodes) {
      expect(node.initialWidth).toBeGreaterThan(0);
      expect(node.initialHeight).toBeGreaterThan(0);
    }
  });
});

describe('a run overlay (UI-3)', () => {
  const overlay = {
    nodes: new Map([
      [
        'changelog',
        {
          state: 'succeeded' as const,
          stateLabel: 'Succeeded',
          verdict: 'pass' as const,
          ran: true,
        },
      ],
      ['checks', { state: 'succeeded' as const, stateLabel: 'Succeeded', ran: true }],
      ['preflight', { state: 'running' as const, stateLabel: 'Running', ran: true }],
      [
        'lead_signoff',
        {
          state: 'pending' as const,
          stateLabel: 'Not taken',
          dimmed: true,
          note: 'Branch not taken',
          ran: false,
        },
      ],
      [
        'cab_review',
        {
          state: 'waiting' as const,
          stateLabel: 'Waiting',
          badge: { kind: 'approval' as const, label: 'Approval · team oncall' },
          current: true,
          ran: true,
        },
      ],
    ]),
  };

  it('draws state, verdict and waiting-on on the cards and stays axe-clean', async () => {
    const { container } = render(
      <div style={{ width: 1000, height: 800 }}>
        <RunbookGraph outline={releaseTrain} overlay={overlay} />
      </div>,
    );
    const changelog = graphNode(container, 'changelog');
    expect(changelog).toHaveAttribute('data-state', 'succeeded');
    expect(within(changelog).getByText('Pass')).toBeInTheDocument();
    expect(within(changelog).getByRole('button', { name: /verdict pass/ })).toBeInTheDocument();
    const cab = graphNode(container, 'cab_review');
    expect(cab).toHaveAttribute('data-current', 'true');
    expect(within(cab).getByText('Approval · team oncall')).toBeInTheDocument();
    const lead = graphNode(container, 'lead_signoff');
    expect(lead).toHaveAttribute('data-dimmed', 'true');
    expect(within(lead).getByText('Branch not taken')).toBeInTheDocument();
    // Cards without an entry still show the run's view of them as nothing.
    expect(graphNode(container, 'canary')).not.toHaveAttribute('data-state');
    await expectNoAxeViolations(container);
  });

  it('hands the side panel to the host', () => {
    const { container } = render(
      <div style={{ width: 1000, height: 800 }}>
        <RunbookGraph
          outline={releaseTrain}
          overlay={overlay}
          selectedId="changelog"
          renderDetails={(node) => <aside aria-label="Run step">{node.id} details</aside>}
        />
      </div>,
    );
    expect(within(container).getByRole('complementary', { name: 'Run step' })).toHaveTextContent(
      'changelog details',
    );
    expect(within(container).queryByRole('complementary', { name: 'Step details' })).toBeNull();
  });
});

describe('UI-2 review items, fixed in UI-3', () => {
  const sized = (node: React.ReactNode) => <div style={{ width: 1000, height: 800 }}>{node}</div>;
  const select = (container: HTMLElement, id: string) =>
    graphNode(container, id).querySelector<HTMLElement>('[data-slot="node-select"]')!;

  it('moves focus into the panel when it opens and back to the card when Esc closes it', async () => {
    const user = userEvent.setup();
    const { container } = render(<RunbookGraph outline={releaseTrain} />);
    select(container, 'migrations').focus();
    await user.keyboard('{Enter}');
    const panel = screen.getByRole('complementary', { name: 'Step details' });
    const heading = within(panel).getByRole('heading', { level: 2 });
    await waitFor(() => expect(heading).toHaveFocus());
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('complementary', { name: 'Step details' })).toBeNull();
    await waitFor(() => expect(select(container, 'migrations')).toHaveFocus());
  });

  it('reaches the open panel without tabbing through every card', async () => {
    const user = userEvent.setup();
    const { container } = render(<RunbookGraph outline={releaseTrain} />);
    expect(screen.queryByRole('button', { name: 'Go to step details' })).toBeNull();
    fireEvent.click(select(container, 'build'));
    // The first stop in the graph, ahead of the cards.
    const jump = screen.getByRole('button', { name: 'Go to step details' });
    const stops = [...container.querySelectorAll<HTMLElement>('button, a[href]')];
    expect(stops.indexOf(jump)).toBeLessThan(stops.indexOf(select(container, 'preflight')));
    select(container, 'preflight').focus();
    jump.focus();
    await user.keyboard('{Enter}');
    expect(
      within(screen.getByRole('complementary', { name: 'Step details' })).getByRole('heading', {
        level: 2,
      }),
    ).toHaveFocus();
  });

  it('follows call and child-run links through the host’s router', () => {
    const go = vi.fn();
    const overlay = {
      nodes: new Map([
        [
          'verify',
          {
            state: 'waiting' as const,
            stateLabel: 'Waiting',
            ran: true,
            child: { href: '/x/exe_CHILD', label: 'Child run' },
          },
        ],
      ]),
    };
    const { container, unmount } = render(
      <RunbookGraph
        outline={releaseTrain}
        callHref={(s) => `/p/demo/runbooks/${s}`}
        onNavigate={go}
      />,
    );
    const call = graphNode(container, 'verify').querySelector<HTMLElement>(
      '[data-slot="call-link"]',
    )!;
    expect(fireEvent.click(call)).toBe(false); // default prevented: no reload
    expect(go).toHaveBeenCalledWith('/p/demo/runbooks/post-deploy-checks');
    // A new-tab click stays the browser's.
    expect(fireEvent.click(call, { metaKey: true })).toBe(true);
    expect(go).toHaveBeenCalledTimes(1);
    unmount();
    const run = render(<RunbookGraph outline={releaseTrain} overlay={overlay} onNavigate={go} />);
    fireEvent.click(
      graphNode(run.container, 'verify').querySelector<HTMLElement>(
        '[data-slot="child-run-link"]',
      )!,
    );
    expect(go).toHaveBeenLastCalledWith('/x/exe_CHILD');
  });

  it('keeps collapsed containers and the viewport across a remount with the same view key', () => {
    const first = render(sized(<RunbookGraph outline={releaseTrain} viewKey="test:a" />));
    fireEvent.click(toggleOf(first.container, 'preflight'));
    expect(first.container.querySelector('[data-node-id="checks"]')).toBeNull();
    first.unmount();
    rememberGraphView('test:a', { viewport: { x: 12, y: -340, zoom: 0.8 } });
    const again = render(sized(<RunbookGraph outline={releaseTrain} viewKey="test:a" />));
    expect(again.container.querySelector('[data-node-id="checks"]')).toBeNull();
    expect(
      again.container.querySelector<HTMLElement>('.react-flow__viewport')?.style.transform,
    ).toBe('translate(12px,-340px) scale(0.8)');
    again.unmount();
    // Another key starts fresh.
    const other = render(sized(<RunbookGraph outline={releaseTrain} viewKey="test:b" />));
    expect(other.container.querySelector('[data-node-id="checks"]')).not.toBeNull();
  });

  it('starts with the minimap folded away, and opens it on request', async () => {
    window.localStorage.removeItem('rb.graph.minimap');
    const user = userEvent.setup();
    render(<RunbookGraph outline={releaseTrain} />);
    expect(screen.queryByLabelText('Overview of the whole graph')).toBeNull();
    const toggle = screen.getByRole('button', { name: 'Show the overview map' });
    await user.click(toggle);
    expect(screen.getByLabelText('Overview of the whole graph')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Hide the overview map' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
  });

  it('closes the panel when a controlling host clears its selection', () => {
    function Host() {
      const [selected, setSelected] = useState<string | undefined>('build');
      return (
        <>
          <button type="button" onClick={() => setSelected(undefined)}>
            Clear
          </button>
          <RunbookGraph outline={releaseTrain} selectedId={selected} onSelect={setSelected} />
        </>
      );
    }
    const { container } = render(<Host />);
    fireEvent.click(select(container, 'verify'));
    expect(screen.getByRole('complementary', { name: 'Step details' })).toHaveTextContent(
      'Post-deploy checks',
    );
    fireEvent.click(screen.getByRole('button', { name: 'Clear' }));
    expect(screen.queryByRole('complementary', { name: 'Step details' })).toBeNull();
  });

  it('names iteration buttons by the step’s title, without a live region on the card', () => {
    const overlay = {
      nodes: new Map([
        [
          'rollout',
          {
            state: 'running' as const,
            stateLabel: 'Running',
            ran: true,
            iteration: { label: 'region 2 of 3', detail: 'us-west', index: 1, keys: ['0', '1'] },
          },
        ],
      ]),
    };
    const { container } = render(<RunbookGraph outline={releaseTrain} overlay={overlay} />);
    const rollout = graphNode(container, 'rollout');
    expect(
      within(rollout).getByRole('button', { name: 'Previous iteration of Roll out per region' }),
    ).toBeInTheDocument();
    expect(
      within(rollout).getByRole('button', { name: 'Next iteration of Roll out per region' }),
    ).toBeInTheDocument();
    expect(rollout.querySelector('[aria-live]')).toBeNull();
  });

  it('keeps a destructive step’s red border during a run', () => {
    const overlay = {
      nodes: new Map([
        ['deploy_region', { state: 'succeeded' as const, stateLabel: 'Succeeded', ran: true }],
      ]),
    };
    const { container } = render(<RunbookGraph outline={releaseTrain} overlay={overlay} />);
    expect(graphNode(container, 'deploy_region')).toHaveClass('border-danger');
  });
});
