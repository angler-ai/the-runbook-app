import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { describe, expect, it } from 'vitest';
import { expectNoAxeViolations } from '../../test/axe';
import { EventPicker } from './EventPicker';
import { ALL_EVENT_KINDS, eventsOfSelection, selectionOf } from './events';

function Harness({ initial = new Set<string>() }: { initial?: Set<string> }) {
  const [selected, setSelected] = useState<ReadonlySet<string>>(initial);
  return (
    <div>
      <EventPicker selected={selected} onChange={setSelected} />
      <output aria-label="selection">{[...selected].sort().join(',')}</output>
    </div>
  );
}

describe('EventPicker', () => {
  it('starts empty, selects per event and per group, and counts', async () => {
    const user = userEvent.setup();
    const { container } = render(<Harness />);
    expect(screen.getByText(`0 of ${ALL_EVENT_KINDS.length} events selected`)).toBeInTheDocument();
    await user.click(screen.getByRole('checkbox', { name: /Approval requested/ }));
    expect(screen.getByLabelText('selection')).toHaveTextContent('approval.requested');
    await user.click(screen.getByRole('checkbox', { name: /^Runs$/ }));
    expect(screen.getByRole('checkbox', { name: /Run started/ })).toBeChecked();
    await user.click(screen.getByRole('button', { name: 'Select all' }));
    expect(
      screen.getByText(`${ALL_EVENT_KINDS.length} of ${ALL_EVENT_KINDS.length} events selected`),
    ).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Deselect all' }));
    expect(screen.getByLabelText('selection')).toBeEmptyDOMElement();
    await expectNoAxeViolations(container);
  });

  it('maps the stored forms both ways: "*" is everything, partial stays explicit', () => {
    expect(selectionOf(['*'])).toEqual(new Set(ALL_EVENT_KINDS));
    expect(eventsOfSelection(new Set(ALL_EVENT_KINDS))).toEqual(['*']);
    expect(eventsOfSelection(new Set(['step.blocked']))).toEqual(['step.blocked']);
    expect(selectionOf(['step.blocked', 'nope.unknown'])).toEqual(new Set(['step.blocked']));
  });
});
