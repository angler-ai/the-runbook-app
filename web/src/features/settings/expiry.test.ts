import { describe, expect, it } from 'vitest';
import { expiryChoices, expiryMs } from './CreatePersonalTokenDialog';

const DAY = 86_400_000;
const NOW = Date.UTC(2026, 10, 21);

describe('personal token expiry presets', () => {
  it('offers every preset with the 90-day default when nothing limits it', () => {
    const choices = expiryChoices(NOW, null);
    expect(choices.options.map((o) => o.value)).toEqual(['7', '30', '90', '365']);
    expect(choices.defaultValue).toBe('90');
    expect(choices.limitMs).toBeNull();
    expect(expiryMs('90', NOW, choices)).toBe(NOW + 90 * DAY);
  });

  it('keeps the default within a longer-lived credential', () => {
    const choices = expiryChoices(NOW, NOW + 200 * DAY);
    expect(choices.options.map((o) => o.value)).toEqual(['7', '30', '90', 'limit']);
    expect(choices.defaultValue).toBe('90');
    expect(choices.limitMs).not.toBeNull();
  });

  it('picks the largest preset a browser session allows', () => {
    const choices = expiryChoices(NOW, NOW + 30 * DAY);
    // 30 days exactly would end after the session once the request is under way.
    expect(choices.options.map((o) => o.value)).toEqual(['7', 'limit']);
    expect(choices.defaultValue).toBe('7');
    expect(expiryMs('limit', NOW, choices)).toBeLessThan(NOW + 30 * DAY);
  });

  it('falls back to the session end when no preset fits', () => {
    const choices = expiryChoices(NOW, NOW + 3 * DAY);
    expect(choices.options.map((o) => o.value)).toEqual(['limit']);
    expect(choices.defaultValue).toBe('limit');
    // A preset chosen earlier never goes past the limit.
    expect(expiryMs('7', NOW, choices)).toBe(choices.limitMs);
  });
});
