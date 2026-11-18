import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { LiveIndicator, stoppedBecause } from './RunHeader';

describe('the live indicator', () => {
  it('says Reconnecting… (not Live) after the stream failed or stalled', () => {
    const { container, rerender } = render(<LiveIndicator status="live" terminal={false} />);
    expect(screen.getByRole('status')).toHaveTextContent('Live');
    rerender(<LiveIndicator status="reconnecting" terminal={false} />);
    expect(screen.getByRole('status')).toHaveTextContent('Reconnecting…');
    expect(container.querySelector('[data-live]')).toBeNull();
  });
});

describe('why a run stopped', () => {
  it('says the server’s step cap, a timeout or the raw reason; nothing for a cancel', () => {
    expect(stoppedBecause('limit.step_runs_exceeded')).toBe(
      'it reached this server’s limit on steps per run.',
    );
    expect(stoppedBecause('timeout')).toBe('it ran out of time.');
    expect(stoppedBecause('something.new')).toBe('something.new');
    expect(stoppedBecause('cancelled')).toBeNull();
    expect(stoppedBecause(undefined)).toBeNull();
  });
});
