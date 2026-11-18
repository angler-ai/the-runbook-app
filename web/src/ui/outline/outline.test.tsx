import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expectNoAxeViolations } from '../../test/axe';
import { RunbookOutline } from './RunbookOutline';
import type { Outline } from '../types';
import postDeployJson from './fixtures/post-deploy.outline.json';

describe('post-deploy.outline.json fixture', () => {
  it('conforms to Outline type contract strictly', () => {
    const outline: Outline = postDeployJson as Outline;
    expect(outline.id).toBe('post-deploy');
    expect(outline.title).toBe('Post-deploy verification');
    expect(outline.stepBudget).toBe(20);
    expect(outline.steps.length).toBeGreaterThan(0);
    expect(outline.finally.length).toBe(1);
    expect(outline.finally[0]!.id).toBe('notify');

    // Verify first step structure
    const smoke = outline.steps[0]!;
    expect(smoke.id).toBe('smoke');
    expect(smoke.kind).toBe('parallel');
    expect(smoke.children).toHaveLength(2);
    expect(smoke.children![0]!.id).toBe('api_health');
    expect(smoke.children![0]!.check).toBe(true);
    expect(smoke.children![1]!.actor).toBe('human');
  });
});

describe('RunbookOutline', () => {
  const outline: Outline = postDeployJson as Outline;

  it('renders outline rows, branch labels, finally section, and is axe-clean', async () => {
    const onSelect = vi.fn();
    const { container } = render(
      <RunbookOutline outline={outline} selectedId="api_health" onSelect={onSelect} />,
    );

    // Tree container
    const tree = screen.getByRole('tree', { name: 'Post-deploy verification Outline' });
    expect(tree).toBeInTheDocument();

    // Steps and hints
    expect(screen.getByText('Smoke tests')).toBeInTheDocument();
    expect(screen.getByText('Run in parallel')).toBeInTheDocument();

    expect(screen.getByText('Health endpoint returns 200')).toBeInTheDocument();
    expect(screen.getAllByText('Agent · check')).toHaveLength(3);

    expect(screen.getByText('Log in as the test user')).toBeInTheDocument();
    expect(screen.getByText('Human · check')).toBeInTheDocument();

    expect(screen.getByText('Error rate per region')).toBeInTheDocument();
    expect(screen.getByText('For each item')).toBeInTheDocument();

    expect(screen.getByText('Wait for errors to settle')).toBeInTheDocument();
    expect(screen.getByText('Repeat up to 3 times')).toBeInTheDocument();

    // Branch labels: a case is labelled by its condition (UI-2)
    expect(screen.getByText(/^Branch: steps\.smoke\.verdict == 'fail' \|\|/)).toBeInTheDocument();
    expect(screen.getByText('Branch: default')).toBeInTheDocument();

    // Finally section
    expect(screen.getByText('Always at the end')).toBeInTheDocument();
    expect(screen.getByText('Post the report to the deploys channel')).toBeInTheDocument();

    // Selected item indicator
    const selectedItem = container.querySelector('[data-node-id="api_health"]');
    expect(selectedItem).toHaveAttribute('aria-selected', 'true');

    await expectNoAxeViolations(container);
  });

  it('expands and collapses technical details disclosure', async () => {
    const user = userEvent.setup();
    render(<RunbookOutline outline={outline} />);

    expect(screen.queryByText('Runbook ID:')).toBeNull();

    const detailsBtn = screen.getByRole('button', { name: /Technical details/ });
    await user.click(detailsBtn);

    expect(screen.getByText('Runbook ID:')).toBeInTheDocument();
    expect(screen.getByText('post-deploy')).toBeInTheDocument();
    expect(screen.getByText('Step Budget:')).toBeInTheDocument();
    expect(screen.getByText('20 steps max')).toBeInTheDocument();

    // Hide details
    await user.click(screen.getByRole('button', { name: /Hide details/ }));
    expect(screen.queryByText('Runbook ID:')).toBeNull();
  });

  it('handles tree keyboard navigation with arrow keys and Enter selection', async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    const { container } = render(
      <RunbookOutline outline={outline} selectedId="smoke" onSelect={onSelect} />,
    );

    const smokeRow = container.querySelector('[data-node-id="smoke"]') as HTMLElement;
    smokeRow.focus();

    // ArrowDown moves to next visible item
    fireEvent.keyDown(smokeRow, { key: 'ArrowDown' });
    const apiHealthRow = container.querySelector('[data-node-id="api_health"]') as HTMLElement;
    expect(document.activeElement).toBe(apiHealthRow);

    // Enter selects the focused item
    fireEvent.keyDown(apiHealthRow, { key: 'Enter' });
    expect(onSelect).toHaveBeenCalledWith('api_health');

    // ArrowUp moves back to previous item
    fireEvent.keyDown(apiHealthRow, { key: 'ArrowUp' });
    expect(document.activeElement).toBe(smokeRow);

    // Collapse node with ArrowLeft
    fireEvent.keyDown(smokeRow, { key: 'ArrowLeft' });
    expect(smokeRow).toHaveAttribute('aria-expanded', 'false');

    // Expand node with ArrowRight
    fireEvent.keyDown(smokeRow, { key: 'ArrowRight' });
    expect(smokeRow).toHaveAttribute('aria-expanded', 'true');

    // Click row directly
    const loginFlowRow = container.querySelector('[data-node-id="login_flow"]') as HTMLElement;
    await user.click(loginFlowRow);
    expect(onSelect).toHaveBeenCalledWith('login_flow');
  });
});

describe('outline expanders (WCAG 2.5.8 target size, 2.4.7 focus visible)', () => {
  const outline: Outline = postDeployJson as Outline;

  // jsdom has no layout: the e2e editor spec measures the rendered box; here
  // the classes that give it are pinned so a regression fails fast.
  it('are at least 24×24 px with a visible focus ring, and keep rows aligned', () => {
    const { container } = render(<RunbookOutline outline={outline} />);
    const expanders = screen.getAllByRole('button', { name: /^(Collapse|Expand) / });
    expect(expanders.length).toBeGreaterThan(0);
    for (const button of expanders) {
      const classes = button.className.split(/\s+/);
      expect(classes).toContain('size-6');
      expect(classes).not.toContain('focus-visible:outline-none');
      expect(classes).toEqual(
        expect.arrayContaining(['focus-visible:outline-2', 'focus-visible:outline-accent']),
      );
    }
    // Leaf rows reserve the same width, so titles line up.
    const spacers = container.querySelectorAll(
      '[data-slot="outline-row"] span[aria-hidden="true"]:first-child',
    );
    for (const spacer of spacers) expect(spacer.className.split(/\s+/)).toContain('size-6');
  });
});
