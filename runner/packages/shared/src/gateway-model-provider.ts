import { traceFetch } from './telemetry.ts';
import type { LanguageModelV4, LanguageModelV4CallOptions, LanguageModelV4GenerateResult, LanguageModelV4StreamPart } from '@ai-sdk/provider';

interface GatewayModelOptions {
  gatewayUrl: string;
  processToken: string;
  capability: () => string;
  maxOutputTokens: number;
  fetch?: typeof fetch;
  streaming?: boolean;
}

export function gatewayModel(profile: string, options: GatewayModelOptions): LanguageModelV4 {
  async function doGenerate(call: LanguageModelV4CallOptions): Promise<LanguageModelV4GenerateResult> {
    const messages = call.prompt.map((message) => {
      if (typeof message.content === 'string') return { role: message.role, content: message.content };
      const text = message.content.map((part) => {
        if (part.type !== 'text') throw new Error('gateway model only accepts text messages');
        return part.text;
      }).join('');
      return { role: message.role, content: text };
    });
    if (call.tools?.length) throw new Error('gateway model does not accept provider tools');
    const response = await traceFetch(options.fetch ?? fetch, `${options.gatewayUrl}/model/generate`, {
      method: 'POST',
      redirect: 'error',
      headers: { authorization: `Bearer ${options.processToken}`, 'content-type': 'application/json' },
      body: JSON.stringify({
        capability: options.capability(),
        profile,
        request: { messages, prompt: messages.map(({ role, content }) => `${role}: ${content}`).join('\n'), max_tokens: call.maxOutputTokens ?? options.maxOutputTokens },
      }),
      ...(call.abortSignal && { signal: call.abortSignal }),
    });
    if (response.status === 402) {
      const body = await response.json() as { error?: { message?: unknown } };
      const message = body.error?.message;
      throw new Error(typeof message === 'string' && /^budget reached: [a-z._:-]+$/.test(message)
        ? message : 'budget reached');
    }
    if (!response.ok) throw new Error('gateway model call failed');
    const result = await response.json() as {
      text: string;
      usage: { input_tokens: number; output_tokens: number; cached_tokens?: number };
    };
    return {
      content: [{ type: 'text', text: result.text }],
      finishReason: { unified: 'other', raw: undefined },
      usage: {
        inputTokens: { total: result.usage.input_tokens, noCache: undefined, cacheRead: result.usage.cached_tokens, cacheWrite: undefined },
        outputTokens: { total: result.usage.output_tokens, text: undefined, reasoning: undefined },
      },
      warnings: [],
    };
  }

  async function relay(call: LanguageModelV4CallOptions) {
    const { abortSignal, headers, ...request } = call;
    const metadata = headers?.['x-runbook-model-attempts'];
    const endgame = headers?.['x-runbook-model-endgame'];
    const response = await traceFetch(options.fetch ?? fetch, `${options.gatewayUrl}/model/stream`, {
      method: 'POST', redirect: 'error', signal: abortSignal,
      headers: { authorization: `Bearer ${options.processToken}`, 'content-type': 'application/json' },
      body: JSON.stringify({ capability: options.capability(), profile,
        ...(metadata !== undefined && { attemptLimit: Number(metadata) }),
        ...(endgame !== undefined && { endgame: endgame === '1' }),
        request: { ...request, maxOutputTokens: request.maxOutputTokens ?? options.maxOutputTokens } }),
    });
    if (response.status === 402) {
      const body = await response.json() as { error?: { message?: unknown } };
      const message = body.error?.message;
      throw new Error(typeof message === 'string' && /^budget reached: [a-z._:-]+$/.test(message)
        ? message : 'budget reached');
    }
    if (!response.ok || !response.body) {
      throw new Error(response.status >= 400 && response.status < 500 && ![408, 409, 429].includes(response.status)
        ? 'model.permanent_failure' : 'gateway model call failed');
    }
    let pending = '';
    return { stream: response.body.pipeThrough(new TextDecoderStream()).pipeThrough(
      new TransformStream<string, LanguageModelV4StreamPart>({
        transform(chunk, controller) {
          pending += chunk;
          let end: number;
          while ((end = pending.indexOf('\n\n')) >= 0) {
            const event = pending.slice(0, end);
            pending = pending.slice(end + 2);
            const [kind, data] = event.split('\n');
            if (kind === 'event: activity') controller.enqueue({ type: 'raw', rawValue: { runnerActivity: true } });
            else if (kind === 'event: attempt' && data?.startsWith('data: ')) {
              controller.enqueue({ type: 'raw', rawValue: { runnerAttempt: Number(data.slice(6)) } });
            } else if (kind === 'event: part' && data?.startsWith('data: ')) {
              const part = JSON.parse(data.slice(6)) as LanguageModelV4StreamPart;
              if (part.type === 'response-metadata' && typeof part.timestamp === 'string') part.timestamp = new Date(part.timestamp);
              controller.enqueue(part);
            } else if (kind === 'event: error') {
              const error = ['secret_in_model_output', 'context_full', 'model.permanent_failure'].find((code) => data === `data: "${code}"`) ??
                'gateway model call failed';
              controller.enqueue({ type: 'error', error: new Error(error) });
            }
          }
        },
      })) };
  }

  return {
    specificationVersion: 'v4',
    provider: 'runbook.gateway',
    modelId: profile,
    supportedUrls: {},
    doGenerate,
    async doStream(call) {
      if (options.streaming) return relay(call);
      const result = await doGenerate(call);
      return {
        stream: new ReadableStream({
          start(controller) {
            controller.enqueue({ type: 'text-start', id: 'text' });
            for (const part of result.content) {
              if (part.type === 'text') controller.enqueue({ type: 'text-delta', id: 'text', delta: part.text });
            }
            controller.enqueue({ type: 'text-end', id: 'text' });
            controller.enqueue({ type: 'finish', usage: result.usage, finishReason: result.finishReason });
            controller.close();
          },
        }),
      };
    },
  };
}
