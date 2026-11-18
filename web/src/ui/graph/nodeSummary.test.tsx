import { render } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import type { OutlineNode } from '../types';
import { NodeSummary } from './RunbookNodeComponent';

const task = (extra: Partial<OutlineNode>): OutlineNode => ({
  id: 'verify',
  title: 'Verify the first data sync',
  path: '/spec/steps/2',
  kind: 'task',
  ...extra,
});

const line = (node: OutlineNode) => render(<NodeSummary node={node} />).container.textContent;

describe('a task card bottom line', () => {
  it('does not repeat the actor the badge already shows', () => {
    expect(line(task({ actor: 'agent', hint: 'Agent' }))).toBe('');
    expect(line(task({ actor: 'human', hint: 'Human' }))).toBe('');
  });

  it('keeps what the hint adds, and the step details', () => {
    expect(line(task({ actor: 'agent', hint: 'Agent · destructive', destructive: true }))).toBe(
      'Destructive',
    );
    expect(line(task({ actor: 'human', assignee: 'team:oncall', checklist: ['a', 'b'] }))).toBe(
      'team:oncall · 2-item checklist',
    );
  });
});
