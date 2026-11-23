import { expect, it } from 'vitest';
import { modelCachePricing } from '../src/model-pricing.ts';

it.each([
  ['claude-opus-5-5', 4, 0.20],
  ['claude-fable-5-2', 8, 0.20],
  ['claude-fable-5-1', 10, 0.25],
  ['claude-sonnet-5-5', 2, 0.10],
  ['claude-haiku-5-5', 0.10, 0.01],
] as const)('uses the listed Claude API cache rate for %s', (model, input, read) => {
  expect(modelCachePricing('anthropic', model, input).cacheReadUsdPerMillion).toBeCloseTo(read);
});

it('prefers configured rates and advertises the Haiku long-context tier', () => {
  expect(modelCachePricing('anthropic', 'claude-haiku-5-5', 0.10, 0.02, 0.4)).toEqual({
    cacheReadUsdPerMillion: 0.02, cacheWriteUsdPerMillion: 0.4, longContext: { aboveInputTokens: 100_000, multiplier: 5 },
  });
  expect(modelCachePricing('openai-compatible', 'claude-haiku-5-5', 0.10).longContext).toBeUndefined();
});
