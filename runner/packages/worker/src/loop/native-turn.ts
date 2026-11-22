import type { LanguageModelV4, LanguageModelV4CallOptions, LanguageModelV4Message, LanguageModelV4StreamPart } from '@ai-sdk/provider';
import { whileActive } from '../lease.ts';
import { deadline, StepStopped } from './limits.ts';

type Assistant = Extract<LanguageModelV4Message, { role: 'assistant' }>;
type Finish = Extract<LanguageModelV4StreamPart, { type: 'finish' }>;
type Chunk = Extract<Assistant['content'][number], { type: 'text' | 'reasoning' }>;

export async function nativeTurn(model: LanguageModelV4, request: LanguageModelV4CallOptions, signal: AbortSignal,
  stallMs: number, activity: () => void, onAttempt: (count: number) => void, onText: (text: string) => void, onReasoning?: (text: string) => void) {
  const stall = deadline(stallMs, 'stream_stall');
  const abortSignal = AbortSignal.any([signal, stall.signal]);
  const content: Assistant['content'] = [];
  const chunks = new Map<string, Chunk>();
  const openTools = new Set<string>();
  const invalidCalls = new Set<string>();
  let finish: Finish | undefined;
  let served = model.modelId;
  let bytes = 0;
  try {
    await whileActive(abortSignal, async () => {
      const { stream } = await model.doStream({ ...request, abortSignal });
      const reader = stream.getReader();
      const cancel = () => { void reader.cancel().catch(() => undefined); };
      abortSignal.addEventListener('abort', cancel, { once: true });
      try {
        abortSignal.throwIfAborted();
        while (true) {
          const next = await reader.read();
          abortSignal.throwIfAborted();
          if (next.done) break;
          stall.touch();
          activity();
          const part = next.value;
          switch (part.type) {
            case 'text-start':
            case 'reasoning-start': {
              const chunk: Chunk = { type: part.type === 'text-start' ? 'text' : 'reasoning', text: '',
                ...(part.providerMetadata && { providerOptions: part.providerMetadata }) };
              chunks.set(part.id, chunk);
              content.push(chunk);
              break;
            }
            case 'text-delta':
            case 'reasoning-delta':
            case 'text-end':
            case 'reasoning-end': {
              const chunk = chunks.get(part.id);
              if (chunk) {
                if ('delta' in part) {
                  chunk.text += part.delta;
                  bytes += Buffer.byteLength(part.delta);
                }
                if (part.providerMetadata) {
                  const metadata = { ...chunk.providerOptions };
                  for (const [name, value] of Object.entries(part.providerMetadata)) {
                    metadata[name] = { ...metadata[name], ...value };
                  }
                  chunk.providerOptions = metadata;
                }
                if (part.type === 'text-end' && chunk.text.trim()) onText(chunk.text);
                if (part.type === 'reasoning-end' && chunk.text.trim()) onReasoning?.(chunk.text);
              }
              break;
            }
            case 'tool-input-start':
              openTools.add(part.id);
              break;
            case 'tool-call': {
              openTools.delete(part.toolCallId);
              bytes += Buffer.byteLength(part.input);
              let input: unknown;
              try { input = JSON.parse(part.input); } catch { /* The tool-result will describe the invalid argument. */ }
              if (!input || typeof input !== 'object' || Array.isArray(input)) {
                invalidCalls.add(part.toolCallId);
                input = {};
              }
              content.push({ type: 'tool-call', toolCallId: part.toolCallId, toolName: part.toolName,
                input, ...(part.providerMetadata && { providerOptions: part.providerMetadata }) });
              break;
            }
            case 'response-metadata':
              served = part.modelId ?? served;
              break;
            case 'raw':
              if (part.rawValue && typeof part.rawValue === 'object' && 'runnerAttempt' in part.rawValue &&
                  typeof part.rawValue.runnerAttempt === 'number') onAttempt(part.rawValue.runnerAttempt);
              break;
            case 'finish':
              finish = part;
              break;
            case 'error':
              throw part.error;
          }
          if (bytes > 1024 * 1024) throw new StepStopped('model_response_too_large');
        }
      } finally {
        abortSignal.removeEventListener('abort', cancel);
        cancel();
      }
    });
    if (!finish) throw new Error('incomplete model stream');
    return { content, finish, served, invalidCalls, partialTool: openTools.size > 0 };
  } finally {
    stall.close();
  }
}
