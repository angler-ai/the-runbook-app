import { describe, expect, it } from 'vitest';
import { defaultRunView, validateRunSearch } from './views';

describe('run views', () => {
  it('opens on Steps on a phone and on Graph elsewhere', () => {
    expect(defaultRunView(true)).toBe('steps');
    expect(defaultRunView(false)).toBe('graph');
  });

  it('keeps an explicit graph choice in the URL, and drops unknown views', () => {
    expect(validateRunSearch({ view: 'graph' })).toEqual({ view: 'graph' });
    expect(validateRunSearch({ view: 'steps' })).toEqual({ view: 'steps' });
    expect(validateRunSearch({ view: 'nope' })).toEqual({});
  });
});
