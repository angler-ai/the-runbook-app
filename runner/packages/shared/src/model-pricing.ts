// Default Claude API cache and long-context rates. Configured provider rates take precedence.
export function modelCachePricing(provider: string, id: string, input: number, read?: number, write?: number) {
  const anthropic = provider === 'anthropic' || provider === 'vertex';
  if (provider === 'vertex' || provider === 'vertex-gemini') id = id.slice(id.lastIndexOf('/') + 1).replace(/@\d{8}$/, '');
  // Opus 5.5 and Sonnet 5.5 read cache at 0.05x input ($0.20 and $0.10 per
  // million), on the Claude API and Vertex alike.
  const readMultiplier = provider === 'vertex-gemini' && id === 'gemini-4-hydrogen' ? 0.05 : !anthropic ? 1 : /^claude-(?:opus|sonnet)-5-5(?:-|@|$)/.test(id) ? 0.05 :
    /^claude-(?:fable|mythos)-5-[12](?:-|$)/.test(id) ? 0.025 : 0.1;
  return {
    cacheReadUsdPerMillion: read ?? input * readMultiplier,
    cacheWriteUsdPerMillion: write ?? input * (anthropic ? 2 : 1),
    longContext: anthropic && /^claude-haiku-5-5(?:-|$)/.test(id)
      ? { aboveInputTokens: 100_000, multiplier: 5 } : undefined,
  };
}
