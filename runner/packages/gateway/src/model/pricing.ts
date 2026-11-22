export interface ModelPricing {
  inputUsdPerMillion: number;
  outputUsdPerMillion: number;
  cacheReadUsdPerMillion?: number;
  cacheWriteUsdPerMillion?: number;
  longContext?: { aboveInputTokens: number; multiplier: number };
}

export interface ModelRequest {
  prompt: string;
  max_tokens: number;
  messages?: { role: 'system' | 'user' | 'assistant'; content: string }[];
}

export function priceMultiplier(profile: ModelPricing, input: number): number {
  return profile.longContext && input > profile.longContext.aboveInputTokens ? profile.longContext.multiplier : 1;
}

export function modelCost(profile: ModelPricing, input: number, output: number, read = 0, write = 0): number {
  const fresh = Math.max(0, input - read - write);
  return priceMultiplier(profile, input) * (fresh * profile.inputUsdPerMillion +
    read * (profile.cacheReadUsdPerMillion ?? profile.inputUsdPerMillion) +
    write * (profile.cacheWriteUsdPerMillion ?? profile.inputUsdPerMillion) +
    output * profile.outputUsdPerMillion) / 1_000_000;
}

export function estimateWorstCase(profile: ModelPricing, request: ModelRequest): number {
  // Count UTF-8 bytes and allow room for framing and provider overhead.
  const inputUsdPerMillion = Math.max(profile.inputUsdPerMillion, profile.cacheReadUsdPerMillion ?? 0, profile.cacheWriteUsdPerMillion ?? 0);
  return modelCost({ ...profile, inputUsdPerMillion }, Buffer.byteLength(request.messages ? JSON.stringify(request.messages) : request.prompt, 'utf8') + 1024, request.max_tokens);
}
