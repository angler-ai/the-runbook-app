import { identifier, inSpan } from '@runbook/runner-shared/telemetry';
import { definitiveProviderRejection } from './stream-fetch.ts';
import type { LanguageModelV4CallOptions, LanguageModelV4StreamPart, LanguageModelV4Usage } from '@ai-sdk/provider';
import { setTimeout as delay } from 'node:timers/promises';
import { releasedForms } from '@runbook/runner-shared/redact';
import { createLogger } from '@runbook/runner-shared/log';
import { HttpError } from '@runbook/runner-shared/http';
import type { ReserveRequest, ReserveResponse, SettleRequest } from '@runbook/runner-shared/contracts/internal';
import type { ModelProfile } from './endpoint.ts';
import { priceMultiplier, type ModelPricing } from './pricing.ts';

export type StreamInput = {
  capability: string;
  profile: string;
  endgame?: boolean;
  attemptLimit?: number;
  request: Omit<LanguageModelV4CallOptions, 'abortSignal'>;
};

const log = createLogger('gateway', { component: 'model' });

const anthropicOptions = new Set([
  'thinking', 'effort', 'taskBudget', 'fallbacks', 'contextManagement', 'cacheControl',
  'anthropicBeta', 'sendReasoning', 'disableParallelToolUse',
]);
const anthropicBetas = new Set([
  'context-management-2025-06-27', 'task-budgets-2026-03-13', 'thinking-display-updates-2026-08-18',
  'server-side-fallback-2026-07-01', 'mid-conversation-system-2026-04-07',
]);

function denied(): never {
  throw new HttpError(400, 'model.option_denied', 'model option is not allowed');
}

export function validateCall(profile: ModelProfile, call: StreamInput['request']) {
  if (!call || !Array.isArray(call.prompt) || !call.prompt.length ||
      !Number.isSafeInteger(call.maxOutputTokens) || call.maxOutputTokens! < 1 ||
      Object.keys(call).some((key) => ![
        'prompt', 'maxOutputTokens', 'tools', 'toolChoice', 'providerOptions',
        'temperature', 'topP', 'topK', 'frequencyPenalty', 'presencePenalty', 'stopSequences', 'seed',
      ].includes(key))) denied();
  if (call.tools !== undefined && (!Array.isArray(call.tools) ||
      call.tools.some((tool) => !tool || tool.type !== 'function'))) denied();
  if (call.toolChoice && !['auto', 'none', 'required', 'tool'].includes(call.toolChoice.type)) denied();
  const inspect = (options: LanguageModelV4CallOptions['providerOptions'], allowed: ReadonlySet<string>, part = false) => {
    if (!options) return;
    for (const [name, values] of Object.entries(options)) {
      if (profile.provider === 'vertex-gemini' && (name === 'googleVertex' || name === 'vertex')) {
        if (!values || typeof values !== 'object' || Array.isArray(values)) denied();
        const keys = Object.keys(values);
        if (part && keys.length === 1 && typeof values.thoughtSignature === 'string') continue;
        const config = values.thinkingConfig as Record<string, unknown> | undefined;
        if (!part && allowed === anthropicOptions && keys.length === 1 && config && typeof config === 'object' &&
            !Array.isArray(config) && Object.keys(config).every((key) => key === 'thinkingLevel' || key === 'includeThoughts') &&
            ['low', 'medium', 'high', 'xhigh'].some((level) => level === config.thinkingLevel) &&
            (config.includeThoughts === undefined || typeof config.includeThoughts === 'boolean')) continue;
        denied();
      }
      if (part && profile.provider === 'openai-compatible' && (name === 'runbook' || name === 'google') &&
          values && typeof values === 'object' && Object.keys(values).length === 1 &&
          typeof values.thoughtSignature === 'string') continue;
      if ((profile.provider !== 'anthropic' && profile.provider !== 'vertex') || name !== 'anthropic' || !values || typeof values !== 'object' || Array.isArray(values) ||
          Object.keys(values).some((key) => !allowed.has(key))) denied();
      const betas = values.anthropicBeta;
      if (betas !== undefined && (!Array.isArray(betas) ||
          betas.some((beta) => typeof beta !== 'string' || !anthropicBetas.has(beta)))) denied();
      const fallbacks = values.fallbacks;
      if (fallbacks !== undefined && (profile.provider === 'vertex' || !Array.isArray(fallbacks) || !fallbacks.length ||
        fallbacks.some((entry) => !entry || typeof entry !== 'object' || Array.isArray(entry) ||
          Object.keys(entry).some((key) => key !== 'model') ||
          !profile.servedPrices?.some((price) => price.model === entry.model)))) denied();
    }
  };
  if (profile.provider === 'vertex-gemini' && (call.frequencyPenalty !== undefined || call.presencePenalty !== undefined)) denied();
  inspect(call.providerOptions, anthropicOptions);
  for (const message of call.prompt) {
    if (!message || !['system', 'user', 'assistant', 'tool'].includes(message.role) ||
        (message.role === 'system' ? typeof message.content !== 'string' : !Array.isArray(message.content))) denied();
    inspect(message.providerOptions, new Set(['cacheControl']));
    if (typeof message.content === 'string') continue;
    for (const part of message.content) {
      if (!part || typeof part !== 'object' || ('providerExecuted' in part && part.providerExecuted)) denied();
      if (part.type === 'tool-result') {
        if (!['text', 'json', 'error-text', 'error-json'].includes(part.output?.type)) denied();
        if ('providerOptions' in part.output) inspect(part.output.providerOptions, new Set(['cacheControl']));
      }
      // The original signed reasoning block must round-trip intact.
      inspect(part.providerOptions, new Set(['cacheControl', 'signature', 'redactedData', 'blockBinding', 'encryptedContent', 'caller', 'toolsetName', 'citations']), true);
      if (part.type === 'file' || part.type === 'reasoning-file' || part.type === 'custom') denied();
    }
  }
  for (const tool of call.tools ?? []) if (tool.type === 'function') inspect(tool.providerOptions, new Set(['cacheControl']));
}

function inputCost(price: ModelPricing, fresh: number, read: number, write: number) {
  return (fresh * price.inputUsdPerMillion + read * (price.cacheReadUsdPerMillion ?? price.inputUsdPerMillion) +
    write * (price.cacheWriteUsdPerMillion ?? price.inputUsdPerMillion)) / 1_000_000;
}

export function usageCost(price: ModelPricing, usage: LanguageModelV4Usage) {
  const { total = 0, cacheRead = 0, cacheWrite = 0 } = usage.inputTokens;
  const fresh = usage.inputTokens.noCache ?? Math.max(0, total - cacheRead - cacheWrite);
  return priceMultiplier(price, total || fresh + cacheRead + cacheWrite) * (inputCost(price, fresh, cacheRead, cacheWrite) +
    (usage.outputTokens.total ?? 0) * price.outputUsdPerMillion / 1_000_000);
}

function mergeUsage(previous: LanguageModelV4Usage | undefined, current: LanguageModelV4Usage): LanguageModelV4Usage {
  if (!previous) return current;
  const input = current.inputTokens;
  const output = current.outputTokens;
  return {
    ...current,
    inputTokens: {
      total: input.total ?? previous.inputTokens.total,
      noCache: input.noCache ?? previous.inputTokens.noCache,
      cacheRead: input.cacheRead ?? previous.inputTokens.cacheRead,
      cacheWrite: input.cacheWrite ?? previous.inputTokens.cacheWrite,
    },
    outputTokens: {
      total: output.total ?? previous.outputTokens.total,
      text: output.text ?? previous.outputTokens.text,
      reasoning: output.reasoning ?? previous.outputTokens.reasoning,
    },
  };
}

function reservationPrice(price: ModelProfile, call: StreamInput['request'], extraTokens = 0) {
  const tokens = Buffer.byteLength(JSON.stringify(call)) + 64 + extraTokens;
  const multiplier = priceMultiplier(price, tokens);
  // The provider may create a fresh cache entry even for a repeated request.
  const inputRate = Math.max(price.inputUsdPerMillion, price.cacheWriteUsdPerMillion ?? 0,
    price.cacheReadUsdPerMillion ?? 0);
  return {
    inputUsd: multiplier * tokens * inputRate / 1_000_000,
    outputRate: multiplier * price.outputUsdPerMillion / 1_000_000,
  };
}

export function reservationInput(price: ModelProfile, call: StreamInput['request']) {
  return reservationPrice(price, call).inputUsd;
}

function statusCode(error: unknown) {
  const status = (error as { statusCode?: unknown } | null)?.statusCode;
  return typeof status === 'number' ? status : undefined;
}

function retryableStatus(status: number) {
  return status === 408 || status === 409 || status === 429 || status >= 500;
}

function retryDelay(error: unknown, attempt: number, status: number | undefined) {
  const info = error as { responseHeaders?: Record<string, string>; isRetryable?: boolean; code?: string };
  if (!((status !== undefined && retryableStatus(status)) ||
      (status === undefined && (info?.isRetryable || error instanceof TypeError ||
        /^(ECONN|EPIPE|ETIMEDOUT|EAI_AGAIN|UND_ERR)/.test(info?.code ?? ''))))) return undefined;
  const header = info.responseHeaders?.['retry-after'];
  const retryAfter = header === undefined ? 0 : /^\d+(\.\d+)?$/.test(header)
    ? Number(header) * 1000 : Date.parse(header) - Date.now();
  return Math.max(Number.isFinite(retryAfter) ? retryAfter : 0, 150 * 2 ** attempt * (1 + Math.random()));
}

export async function streamModel(input: StreamInput, profile: ModelProfile, deps: {
  signal: AbortSignal;
  availableMs: number;
  reserve(request: ReserveRequest): Promise<ReserveResponse | { level: string }>;
  settle(request: SettleRequest): Promise<void>;
  invoke(call: LanguageModelV4CallOptions, activity: () => void, observe: (model: string | undefined, usage: LanguageModelV4Usage) => void, dispatched: () => void, respond: (status: number) => void):
    PromiseLike<{ stream: ReadableStream<LanguageModelV4StreamPart> }>;
  finished(): void;
  secrets: string[];
}) {
  const call = input.request;
  validateCall(profile, call);
  const attempts = input.attemptLimit ?? 3;
  if (!Number.isSafeInteger(attempts) || attempts < 1 || attempts > 3 ||
      (input.endgame !== undefined && typeof input.endgame !== 'boolean')) denied();
  const deadline = performance.now() + deps.availableMs;
  const fallbacks = call.providerOptions?.anthropic?.fallbacks;
  const prices = Array.isArray(fallbacks) ? (profile.servedPrices ?? []).filter((price) =>
    fallbacks.some((entry) => entry && typeof entry === 'object' && 'model' in entry && entry.model === price.model)) : [];
  const candidates = [profile, ...prices.map((price) => ({ ...profile, ...price, longContext: price.longContext }))];
  const estimates = candidates.map((price) => reservationPrice(price, call));
  const inputUsd = Math.max(...estimates.map(({ inputUsd }) => inputUsd));
  const rate = Math.max(...estimates.map(({ outputRate }) => outputRate));
  const maxOutput = Math.min(call.maxOutputTokens!, profile.maxOutputTokens);
  // Allow for the assistant turn, a full tool-result batch, and the finish instruction.
  const finishHeadroom = input.endgame === false ? Math.max(...candidates.map((price) => {
    const extraTokens = maxOutput + 96 * 1024 + 512;
    const final = reservationPrice(price, call, extraTokens);
    return final.inputUsd + Math.min(4096, profile.maxOutputTokens) * final.outputRate;
  })) : undefined;
  const reservation = await deps.reserve({ capability: input.capability, kind: 'model',
    amount_usd: inputUsd + maxOutput * rate, minimum_usd: inputUsd + Math.min(32, maxOutput) * rate,
    ...(finishHeadroom !== undefined && { headroom_usd: finishHeadroom }),
  });
  if ('level' in reservation) {
    const level = /^(step|run|agent|project|group|org)([._:-](day|month))?$/.test(reservation.level) ? reservation.level : 'unknown';
    throw new HttpError(402, 'model.budget_reached', `budget reached: ${level}`);
  }
  const affordable = rate && reservation.amount_usd !== undefined
    ? Math.floor((reservation.amount_usd - inputUsd + 1e-10) / rate) : maxOutput;
  const abort = new AbortController();
  const signal = AbortSignal.any([deps.signal, abort.signal]);
  const encode = new TextEncoder();
  const body = new ReadableStream<Uint8Array>({
    start: (controller) => inSpan('model.call', async (span) => {
      let usage: LanguageModelV4Usage | undefined;
      let served = profile.model;
      let reportedModel: string | undefined;
      let finish: Extract<LanguageModelV4StreamPart, { type: 'finish' }> | undefined;
      let received = false;
      let dispatched = false;
      let responseStatus: number | undefined;
      let emitted = false;
      let relayedBytes = 0;
      const relayedInputs = new Map<string, number>();
      let failed = false;
      const send = (event: string, data: unknown) => {
        if (!signal.aborted) controller.enqueue(encode.encode(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`));
      };
      const relay = (part: LanguageModelV4StreamPart) => {
        if (signal.aborted) return;
        if (part.type === 'text-delta' || part.type === 'reasoning-delta') relayedBytes += Buffer.byteLength(part.delta);
        if (part.type === 'tool-input-delta') {
          const bytes = Buffer.byteLength(part.delta);
          relayedInputs.set(part.id, (relayedInputs.get(part.id) ?? 0) + bytes);
          relayedBytes += bytes;
        }
        if (part.type === 'tool-call') {
          relayedBytes += Math.max(0, Buffer.byteLength(part.input) - (relayedInputs.get(part.toolCallId) ?? 0));
        }
        send('part', part);
      };
      const parts: { data: LanguageModelV4StreamPart; check: string; channel: string }[] = [];
      const closedChannels = new Set<string>();
      let patterns: string[] = [];
      let window = 0;
      const emit = (part: LanguageModelV4StreamPart) => {
        if (part.type === 'error') throw part.error;
        if (part.type === 'finish') usage = mergeUsage(usage, part.usage);
        if (part.type === 'finish' && part.finishReason.unified === 'interrupted') throw new Error('incomplete model stream');
        if (part.type === 'response-metadata' && part.modelId) served = reportedModel = part.modelId;
        const check = part.type === 'text-delta' || part.type === 'reasoning-delta' || part.type === 'tool-input-delta' ? part.delta : '';
        const channel = 'id' in part && /^(text|reasoning|tool-input)-(start|delta|end)$/.test(part.type)
          ? `${part.type.slice(0, part.type.lastIndexOf('-'))}:${part.id}` : '';
        if (channel && part.type.endsWith('-end')) closedChannels.add(channel);
        const text = parts.filter((part) => part.channel === channel).map((part) => part.check).join('') + check;
        const json = JSON.stringify(part);
        if (patterns.some((secret) => text.includes(secret) || json.includes(JSON.stringify(secret).slice(1, -1)) ||
            (part.type === 'tool-call' && part.input.includes(secret)))) {
          throw new Error('secret_in_model_output');
        }
        if (part.type === 'finish') { finish = part; return; }
        emitted ||= part.type === 'text-delta' || part.type === 'reasoning-delta' || part.type === 'tool-call' || part.type === 'tool-input-delta';
        parts.push({ data: part, check, channel });
        while (parts.length) {
          const first = parts[0]!;
          if (first.check && !closedChannels.has(first.channel) && parts.filter((part) => part.channel === first.channel).reduce((n, part) => n + part.check.length, 0) - first.check.length < window - 1) break;
          relay(parts.shift()!.data);
        }
      };
      try {
        for (let attempt = 0; attempt < attempts; attempt++) {
          try {
            signal.throwIfAborted();
            // Report attempts separately so the worker can enforce the same retry cap.
            if (input.attemptLimit !== undefined) send('attempt', attempt + 1);
            const { stream } = await deps.invoke({ ...call, maxOutputTokens: Math.min(maxOutput, affordable), abortSignal: signal }, () => {
              received = true;
              send('activity', null);
            }, (model, partial) => {
              if (model) served = model;
              usage = mergeUsage(usage, partial);
            }, () => { dispatched = true; }, (status) => { responseStatus = status; });
            dispatched = true;
            patterns = [...new Set(deps.secrets.flatMap(releasedForms))];
            window = Math.max(0, ...patterns.map((value) => value.length));
            const reader = stream.getReader();
            const cancel = () => { void reader.cancel().catch(() => undefined); };
            signal.addEventListener('abort', cancel, { once: true });
            try {
              signal.throwIfAborted();
              while (true) {
                const next = await reader.read();
                signal.throwIfAborted();
                if (next.done) break;
                emit(next.value);
              }
            } finally {
              signal.removeEventListener('abort', cancel);
              cancel();
            }
            if (!finish) throw new Error('incomplete model stream');
            // Vertex exposes modelVersion on the wire, but its SDK metadata
            // may carry only a response ID. Preserve the other parts as sent.
            if (profile.provider === 'vertex-gemini' && served !== reportedModel) {
              emit({ type: 'response-metadata', modelId: served });
            }
            break;
          } catch (error) {
            const status = responseStatus ?? statusCode(error);
            if (definitiveProviderRejection(status) && !received && !emitted && !usage) dispatched = false;
            const wait = retryDelay(error, attempt, status);
            if ((dispatched && status === undefined) || received || emitted || usage || wait === undefined ||
                attempt === attempts - 1 || performance.now() + wait >= deadline) throw error;
            dispatched = false;
            responseStatus = undefined;
            log.warn('Retrying provider stream', { provider: profile.provider, model: profile.model,
              status: statusCode(error), attempt: attempt + 1, delay_ms: Math.ceil(wait) });
            const heartbeat = setInterval(() => send('activity', null), 250);
            try { await delay(wait, undefined, { signal }); }
            finally { clearInterval(heartbeat); }
          }
        }
      } catch (error) {
        failed = true;
        const status = responseStatus ?? statusCode(error);
        const message = error instanceof Error ? error.message : String(error);
        const code = signal.aborted ? 'model.interrupted' : message === 'secret_in_model_output' ? message :
          /context (window|length)|prompt is too long|maximum context length|input token count.*exceeds|maximum (number of )?input tokens/i.test(message) ? 'context_full' :
          status !== undefined && status >= 400 && status < 500 && !retryableStatus(status) ? 'model.permanent_failure' : 'model.call_failed';
        if (!signal.aborted) log.warn('Provider stream failed', { provider: profile.provider, model: profile.model, code, status });
        send('error', code);
      }
      const price = served === profile.model ? profile : profile.servedPrices?.find((item) => item.model === served);
      if (!finish && usage && relayedBytes) {
        usage = { ...usage, outputTokens: { ...usage.outputTokens,
          total: Math.max(usage.outputTokens.total ?? 0, Math.ceil(relayedBytes / 3)) } };
      }
      const observed = usage && (usage.inputTokens.total !== undefined || usage.outputTokens.total !== undefined) ? usage : undefined;
      const reserved = reservation.amount_usd ?? inputUsd + maxOutput * rate;
      const incomplete = failed && !finish && (responseStatus === undefined || (responseStatus >= 200 && responseStatus < 300));
      const pending = dispatched && (incomplete ||
        !observed || !price || (observed.inputTokens.total === undefined && !observed.outputTokens.total));
      if (pending) log.warn('Model usage pending reconciliation', { reservation_id: reservation.reservation_id });
      const measured = observed && price ? usageCost(price, observed) : 0;
      const model = identifier(price?.model ?? profile.model);
      if (model) span.setAttribute('gen_ai.response.model', model);
      span.setAttribute('gen_ai.usage.input_tokens', observed?.inputTokens.total ?? 0);
      span.setAttribute('gen_ai.usage.output_tokens', observed?.outputTokens.total ?? 0);
      span.setAttribute('runbook.model.cost_usd', pending ? Math.max(reserved, measured) : measured);
      try {
        await deps.settle({ reservation_id: reservation.reservation_id,
          actual_usd: pending ? Math.max(reserved, measured) : measured,
          ...(pending && { usage_pending: true }),
          usage: observed ? { input_tokens: observed.inputTokens.total ?? 0, output_tokens: observed.outputTokens.total ?? 0,
            cached_tokens: observed.inputTokens.cacheRead ?? 0, cache_write_tokens: observed.inputTokens.cacheWrite ?? 0 } : {},
        });
        if (!failed && finish && !signal.aborted) {
          if (!price) send('error', 'model.unpriced_served_model');
          else {
            deps.finished();
            for (const part of parts) send('part', part.data);
            send('part', finish);
          }
        }
      } catch {
        send('error', 'model.settlement_failed');
      }
      if (!abort.signal.aborted) controller.close();
    }),
    cancel() { abort.abort(); },
  });
  return { body, headers: { 'content-type': 'text/event-stream', 'x-content-type-options': 'nosniff' } };
}
