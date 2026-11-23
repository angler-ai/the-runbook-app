import { createHash } from 'node:crypto';
import type { LanguageModelV4, LanguageModelV4CallOptions, LanguageModelV4StreamPart } from '@ai-sdk/provider';

export interface ModelCall {
  requestHash: string;
  parts: LanguageModelV4StreamPart[];
  error?: string;
}

export function hashValue(value: unknown): string {
  const canonical = JSON.stringify(value, (_key, value: unknown) => {
    if (value && typeof value === 'object' && !Array.isArray(value)) return Object.fromEntries(Object.entries(value).sort());
    return value;
  });
  return createHash('sha256').update(canonical ?? 'undefined').digest('hex');
}

export function hashRequest({ abortSignal: _signal, ...request }: LanguageModelV4CallOptions): string {
  return hashValue({ ...request, prompt: request.prompt.map((message) => {
    if (message.role !== 'tool') return message;
    return { ...message, content: message.content.map((part) => {
      if (part.type !== 'tool-result') return part;
      const { output } = part;
      if (output.type !== 'text' && output.type !== 'error-text') return part;
      // Normalize only the generated outer wrapper. Preserve every byte of the tool payload.
      const match = /^(\[call \d+\]\n--- Untrusted data from [^\n]+) \[([a-f0-9]{32})\] ---\n([\s\S]*)\n--- End untrusted data \[\2\] ---$/.exec(output.value);
      if (!match) return part;
      return { ...part, output: { ...output, value: match[1] + ' ---\n' + match[3] + '\n--- End untrusted data ---' } };
    }) };
  }) });
}

export function streamingModel(modelId: string, doStream: LanguageModelV4['doStream']): LanguageModelV4 {
  return { specificationVersion: 'v4', provider: 'runbook-eval', modelId, supportedUrls: {}, doStream,
    async doGenerate() { throw new Error('expected a streaming model call'); } };
}

export function partStream(parts: readonly LanguageModelV4StreamPart[]) {
  return new ReadableStream<LanguageModelV4StreamPart>({
    start(controller) {
      for (const part of parts) controller.enqueue(part);
      controller.close();
    },
  });
}

function message(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

export function recordModel(model: LanguageModelV4, beforeCall?: (call: LanguageModelV4CallOptions) => void) {
  const calls: ModelCall[] = [];
  const wrapped = streamingModel(model.modelId, async (request) => {
    const call: ModelCall = { requestHash: hashRequest(request), parts: [] };
    beforeCall?.(request);
    calls.push(call);
    try {
      const result = await model.doStream(request);
      const stream = result.stream.pipeThrough(new TransformStream({
        transform(part: LanguageModelV4StreamPart, controller) {
          call.parts.push(part.type === 'error' ? { ...part, error: message(part.error) } : part);
          controller.enqueue(part);
        },
      }));
      const reader = stream.getReader();
      return { ...result, stream: new ReadableStream<LanguageModelV4StreamPart>({
        async pull(controller) {
          try {
            const next = await reader.read();
            if (next.done) controller.close();
            else controller.enqueue(next.value);
          } catch (error) {
            call.error = message(error);
            controller.error(error);
          }
        },
        cancel: (reason) => reader.cancel(reason),
      }) };
    } catch (error) {
      call.error = message(error);
      throw error;
    }
  });
  return { model: { ...wrapped, provider: model.provider, supportedUrls: model.supportedUrls }, calls };
}

export function replayModel(modelId: string, calls: readonly ModelCall[], provider = 'runbook-eval') {
  let index = 0;
  const divergences: string[] = [];
  const model = streamingModel(modelId, async (request) => {
    const actual = hashRequest(request);
    const call = calls[index++];
    if (actual !== call?.requestHash) {
      const detail = `Replay divergence at call ${index}: expected ${call?.requestHash ?? 'no call'}, got ${actual}`;
      divergences.push(detail);
      throw new Error(detail);
    }
    if (call.error && !call.parts.length) throw new Error(call.error);
    let cursor = 0;
    return { stream: call.error ? new ReadableStream<LanguageModelV4StreamPart>({
      pull(controller) {
        const part = call.parts[cursor++];
        if (part) controller.enqueue(part);
        else controller.error(new Error(call.error));
      },
    }) : partStream(call.parts) };
  });
  return {
    model: { ...model, provider },
    assertComplete() {
      if (divergences.length) throw new Error(divergences.join('\n'));
      if (index !== calls.length) throw new Error(`Replay divergence: consumed ${index} of ${calls.length} calls`);
    },
  };
}
