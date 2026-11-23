import type { LanguageModelV4Usage } from '@ai-sdk/provider';

const MAX_EVENT_BYTES = 1024 * 1024;
const MAX_RESPONSE_BYTES = 16 * 1024 * 1024;

export function streamResponseLimit(profile: { maxOutputTokens: number; maxResponseBytes?: number }): number {
  // Token-sized streaming events can carry much more framing than text.
  return profile.maxResponseBytes ?? MAX_RESPONSE_BYTES + 1024 * profile.maxOutputTokens;
}

function limitResponse(response: Response, maxBytes: number, inspect?: (bytes: Uint8Array) => void): Response {
  if (!response.body) return response;
  let responseBytes = 0;
  const body = response.body.pipeThrough(new TransformStream<Uint8Array, Uint8Array>({
    transform(bytes, controller) {
      responseBytes += bytes.byteLength;
      if (responseBytes > maxBytes) throw new Error('provider stream exceeds response limit');
      inspect?.(bytes);
      controller.enqueue(bytes);
    },
  }));
  return new Response(body, { status: response.status, statusText: response.statusText, headers: response.headers });
}

export const boundedProviderFetch = (fetcher: typeof fetch, respond?: (status: number) => void): typeof fetch =>
  async (input, init) => {
    const response = await fetcher(input, init);
    respond?.(response.status);
    return limitResponse(response, MAX_RESPONSE_BYTES);
  };

export function definitiveProviderRejection(status: number | undefined): boolean {
  return status !== undefined && status >= 400 && status < 600;
}

// Observe wire activity before the SDK drops pings or coalesces provider events.
export function observeProviderFetch(fetcher: typeof fetch, activity: () => void,
  observe: (model: string | undefined, usage: LanguageModelV4Usage) => void, dispatched?: () => void, maxBytes = MAX_RESPONSE_BYTES, respond?: (status: number) => void): typeof fetch {
  return async (input, init) => {
    init?.signal?.throwIfAborted();
    dispatched?.();
    const response = await fetcher(input, init);
    respond?.(response.status);
    if (!response.ok || !response.body) return limitResponse(response, Math.min(maxBytes, MAX_RESPONSE_BYTES));
    const decoder = new TextDecoder();
    let buffer = '';
    let eventBytes = 0;
    let lineBytes = 0;
    let lastByte = 0;
    let model: string | undefined;
    let fresh: number | undefined;
    let read: number | undefined;
    let write: number | undefined;
    let output: number | undefined;
    function inspect(line: string) {
      if (!line.startsWith('data:')) return;
      try {
        const event = JSON.parse(line.slice(5)) as {
          model?: string;
          modelVersion?: string;
          usageMetadata?: { promptTokenCount?: number; cachedContentTokenCount?: number;
            candidatesTokenCount?: number; thoughtsTokenCount?: number };
          usage?: Record<string, number> & {
            prompt_tokens_details?: { cached_tokens?: number };
            completion_tokens_details?: { reasoning_tokens?: number };
          };
          message?: { model?: string; usage?: Record<string, number> };
        };
        model = event.message?.model ?? event.model ?? model;
        if (event.modelVersion) model = event.modelVersion;
        if (event.usageMetadata) {
          const { promptTokenCount, cachedContentTokenCount = 0, candidatesTokenCount, thoughtsTokenCount } = event.usageMetadata;
          if (promptTokenCount !== undefined) observe(model, {
            inputTokens: { total: promptTokenCount, noCache: promptTokenCount - cachedContentTokenCount,
              cacheRead: cachedContentTokenCount, cacheWrite: 0 },
            outputTokens: { total: (candidatesTokenCount ?? 0) + (thoughtsTokenCount ?? 0),
              text: candidatesTokenCount, reasoning: thoughtsTokenCount },
          });
          return;
        }
        const usage = event.message?.usage ?? event.usage;
        if (!usage) return;
        read = usage.cache_read_input_tokens ??
          (event.usage?.prompt_tokens_details && event.usage.prompt_tokens_details.cached_tokens) ?? read;
        write = usage.cache_creation_input_tokens ?? write;
        fresh = usage.input_tokens ?? (usage.prompt_tokens === undefined ? fresh : usage.prompt_tokens - (read ?? 0));
        output = usage.output_tokens ?? usage.completion_tokens ?? output;
        if (fresh === undefined && read === undefined && write === undefined && output === undefined) return;
        observe(model, {
          inputTokens: { total: fresh === undefined && read === undefined && write === undefined
            ? undefined : (fresh ?? 0) + (read ?? 0) + (write ?? 0), noCache: fresh, cacheRead: read, cacheWrite: write },
          outputTokens: { total: output, text: undefined, reasoning: event.usage?.completion_tokens_details?.reasoning_tokens },
        });
      } catch {
        // The SDK owns parsing and reporting malformed upstream events.
      }
    }
    return limitResponse(response, maxBytes, (bytes) => {
      // Check sizes before buffering or forwarding this chunk. Blank lines
      // end SSE events, including CRLF split across transport chunks.
      for (const byte of bytes) {
        eventBytes++;
        if (eventBytes > MAX_EVENT_BYTES) throw new Error('provider stream exceeds event limit');
        if (byte === 0x0a) {
          if (lineBytes === 0 || (lineBytes === 1 && lastByte === 0x0d)) eventBytes = 0;
          lineBytes = 0;
        } else {
          lineBytes++;
        }
        lastByte = byte;
      }
      if (bytes.length) activity();
      buffer += decoder.decode(bytes, { stream: true });
      const lines = buffer.split('\n');
      buffer = lines.pop() ?? '';
      for (const line of lines) inspect(line);
    });
  };
}
