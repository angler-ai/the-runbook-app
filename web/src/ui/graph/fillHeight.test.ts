import { describe, expect, it } from 'vitest';
import { FILL_BOTTOM_PX, FILL_MIN_PX, fillHeight } from './fillHeight';

describe('fillHeight', () => {
  it('reaches the bottom of a tall screen from where the graph starts', () => {
    expect(fillHeight(1440, 400, 0)).toBe(1440 - 400 - FILL_BOTTOM_PX);
  });

  it('measures from the top of the page, not the scrolled position', () => {
    expect(fillHeight(1440, 100, 300)).toBe(1440 - 400 - FILL_BOTTOM_PX);
  });

  it('keeps a usable minimum on a short screen', () => {
    expect(fillHeight(700, 450, 0)).toBe(FILL_MIN_PX);
  });
});
