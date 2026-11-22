import { writeFileSync } from 'node:fs';

interface GeminiUsage {
  promptTokenCount?: number;
  candidatesTokenCount?: number;
  thoughtsTokenCount?: number;
  totalTokenCount?: number;
  cachedContentTokenCount?: number;
}

export function guardGeminiFetch(fetcher: typeof fetch, config: {
  endpoint: string; model: string; cap: number; input: number; output: number; cacheRead: number; maxOutput: number;
}, save: (spent: number, denied: boolean) => void): typeof fetch {
  let spent = 0;
  let denied = false;
  return async (input, init) => {
    const url = new URL(input instanceof Request ? input.url : input);
    if (!/(?:aiplatform|generativelanguage)(?:\.[\w-]+)*\.googleapis\.com$/.test(url.hostname) ||
        !/:(?:streamGenerateContent|generateContent)$/.test(url.pathname)) return fetcher(input, init);
    const target = url.origin + url.pathname;
    if (target !== `${config.endpoint}/${config.model}:streamGenerateContent` &&
        target !== `${config.endpoint}/${config.model}:generateContent`) {
      return new Response('Unexpected eval model endpoint', { status: 403 });
    }
    if (typeof init?.body !== 'string') return new Response('Missing model request body', { status: 400 });
    const body = JSON.parse(init.body) as { generationConfig?: { maxOutputTokens?: number }; continuationToken?: string };
    const max = body.generationConfig?.maxOutputTokens ?? config.maxOutput;
    if (!Number.isSafeInteger(max) || max < 1 || max > config.maxOutput || body.continuationToken) {
      return new Response('Invalid eval model output limit', { status: 400 });
    }
    body.generationConfig = { ...body.generationConfig, maxOutputTokens: max };
    const request = JSON.stringify(body);
    // Charge the uncached byte bound and the entire output allowance before I/O.
    const reserve = Math.ceil((Buffer.byteLength(request) + 1024) * config.input + max * config.output);
    if (spent + reserve > Math.floor(config.cap * 1_000_000 + 1e-7)) {
      denied = true;
      save(spent / 1_000_000, denied);
      return new Response('Eval model budget exceeded', { status: 402 });
    }
    spent += reserve;
    save(spent / 1_000_000, denied);
    const response = await fetcher(input, { ...init, body: request, redirect: 'error' });
    if (!response.ok || !response.body) return response;
    // Buffer one bounded model turn so usage is settled before the CLI sees it.
    const text = await response.text();
    const events = response.headers.get('content-type')?.includes('text/event-stream')
      ? text.split(/\r?\n/).flatMap((line) => line.startsWith('data: ') && line.slice(6) !== '[DONE]' ? [line.slice(6)] : [])
      : [text];
    const usage = events.map((event) => (JSON.parse(event) as { usageMetadata?: GeminiUsage }).usageMetadata)
      .findLast((value) => value?.totalTokenCount !== undefined && value.promptTokenCount !== undefined);
    if (usage) {
      const prompt = usage.promptTokenCount!;
      const cached = usage.cachedContentTokenCount ?? 0;
      const output = Math.max((usage.candidatesTokenCount ?? 0) + (usage.thoughtsTokenCount ?? 0), usage.totalTokenCount! - prompt);
      spent += Math.ceil((prompt - cached) * config.input + cached * config.cacheRead + output * config.output) - reserve;
      save(spent / 1_000_000, denied);
    }
    return new Response(text, { status: response.status, statusText: response.statusText, headers: response.headers });
  };
}

const ledger = process.env.RUNBOOK_EVAL_GEMINI_LEDGER;
const limits = process.env.RUNBOOK_EVAL_GEMINI_LIMITS;
if (ledger && limits) {
  globalThis.fetch = guardGeminiFetch(globalThis.fetch, JSON.parse(limits), (spent, denied) => {
    writeFileSync(ledger, JSON.stringify({ spent, denied }), { mode: 0o600 });
  });
}
