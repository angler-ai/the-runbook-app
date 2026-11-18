import { describe, expect, it } from 'vitest';
import { formatDeadlineLeft, formatDuration, formatRelativeTime } from './format';

describe('formatDuration', () => {
  const cases: Array<[number, string]> = [
    [0, '0s'],
    [500, '<1s'],
    [999, '<1s'],
    [1000, '1s'],
    [12_000, '12s'],
    [59_000, '59s'],
    [60_000, '1m'],
    [72_000, '1m 12s'],
    [120_000, '2m'],
    [3_599_000, '59m 59s'],
    [3_600_000, '1h'],
    [3_660_000, '1h 1m'],
    [7_200_000, '2h'],
    [7_500_000, '2h 5m'],
    [86_400_000, '1d'],
    [90_000_000, '1d 1h'],
    [172_800_000, '2d'],
    [259_200_000, '3d'],
    // Negative duration
    [-500, '-<1s'],
    [-1000, '-1s'],
    [-72_000, '-1m 12s'],
    [-7_500_000, '-2h 5m'],
    [-90_000_000, '-1d 1h'],
  ];

  it.each(cases)('formatDuration(%i) -> %s', (input, expected) => {
    expect(formatDuration(input)).toBe(expected);
  });

  it('handles non-finite numbers safely', () => {
    expect(formatDuration(NaN)).toBe('0s');
    expect(formatDuration(Infinity)).toBe('0s');
  });
});

describe('formatRelativeTime', () => {
  const now = 1_700_000_000_000;

  const pastCases: Array<[number, string]> = [
    [now, 'just now'],
    [now - 500, 'just now'],
    [now - 4000, 'just now'],
    [now - 5000, '5s ago'],
    [now - 45_000, '45s ago'],
    [now - 60_000, '1m ago'],
    [now - 1_080_000, '18m ago'],
    [now - 3_600_000, '1h ago'],
    [now - 7_200_000, '2h ago'],
    [now - 86_400_000, '1d ago'],
    [now - 259_200_000, '3d ago'],
  ];

  it.each(pastCases)('past: formatRelativeTime(%i, now) -> %s', (target, expected) => {
    expect(formatRelativeTime(target, now)).toBe(expected);
  });

  const futureCases: Array<[number, string]> = [
    [now + 2000, 'in a few seconds'],
    [now + 10_000, 'in 10s'],
    [now + 300_000, 'in 5m'],
    [now + 7_200_000, 'in 2h'],
    [now + 86_400_000, 'in 1d'],
    [now + 259_200_000, 'in 3d'],
  ];

  it.each(futureCases)('future: formatRelativeTime(%i, now) -> %s', (target, expected) => {
    expect(formatRelativeTime(target, now)).toBe(expected);
  });

  it('accepts Date and ISO string inputs', () => {
    const d = new Date(now - 60_000);
    expect(formatRelativeTime(d, now)).toBe('1m ago');
    expect(formatRelativeTime(d.toISOString(), now)).toBe('1m ago');
  });

  it('handles invalid inputs safely', () => {
    expect(formatRelativeTime('invalid-date', now)).toBe('');
  });
});

describe('formatDeadlineLeft', () => {
  const now = 1_700_000_000_000;

  const futureCases: Array<[number, string]> = [
    [now + 500, '<1s left'],
    [now + 1000, '1s left'],
    [now + 45_000, '45s left'],
    [now + 60_000, '1m left'],
    [now + 1_800_000, '30m left'],
    [now + 3_600_000, '1h left'],
    [now + 7_200_000, '2h left'],
    [now + 86_400_000, '1d left'],
    [now + 172_800_000, '2d left'],
  ];

  it.each(futureCases)('future: formatDeadlineLeft(%i, now) -> %s', (deadline, expected) => {
    expect(formatDeadlineLeft(deadline, now)).toBe(expected);
  });

  const pastCases: Array<[number, string]> = [
    [now, 'expired'],
    [now - 500, 'expired'],
    [now - 1000, 'overdue by 1s'],
    [now - 45_000, 'overdue by 45s'],
    [now - 60_000, 'overdue by 1m'],
    [now - 300_000, 'overdue by 5m'],
    [now - 7_200_000, 'overdue by 2h'],
    [now - 86_400_000, 'overdue by 1d'],
    [now - 259_200_000, 'overdue by 3d'],
  ];

  it.each(pastCases)('past: formatDeadlineLeft(%i, now) -> %s', (deadline, expected) => {
    expect(formatDeadlineLeft(deadline, now)).toBe(expected);
  });

  it('accepts Date and ISO string inputs', () => {
    const d = new Date(now + 60_000);
    expect(formatDeadlineLeft(d, now)).toBe('1m left');
    expect(formatDeadlineLeft(d.toISOString(), now)).toBe('1m left');
  });

  it('handles invalid inputs safely', () => {
    expect(formatDeadlineLeft('invalid-date', now)).toBe('');
  });
});
