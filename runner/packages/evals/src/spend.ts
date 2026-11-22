import { randomUUID } from 'node:crypto';
import type { ModelProfile } from '@runbook/runner-gateway/model/endpoint';
import { generateModel } from '@runbook/runner-gateway/model/endpoint';
import { modelCost, priceMultiplier, type ModelRequest } from '@runbook/runner-gateway/model/pricing';
import { streamModel, type StreamInput } from '@runbook/runner-gateway/model/stream';
import { observeProviderFetch, streamResponseLimit } from '@runbook/runner-gateway/model/stream-fetch';
import { invokeModel, providerModel } from '@runbook/runner-gateway/model/providers';
import { gatewayModel } from '@runbook/runner-shared/gateway-model-provider';
import { modelCachePricing } from '@runbook/runner-shared/model-pricing';
import { HttpError } from '@runbook/runner-shared/http';
import type { SubjectId } from './baselines.ts';
import type { Task } from './tasks.ts';
import { runOneArchive, runOneCosts, runOneSource } from './run-one-costs.ts';

const profileRows = {
  // Current models and list prices per million tokens (2026-11). Haiku 5.5's
  // price is for prompts up to 100K tokens ($0.50 / $2.50 beyond).
  cheap: ['claude-haiku-5-5', 0.1, 0.5],
  default: ['claude-sonnet-5-5', 2, 10],
  deep: ['claude-opus-5-5', 4, 20],
} as const;
export type ProfileId = keyof typeof profileRows;

// Vertex global USD per million tokens. Add an explicit row before using --model.
const geminiPrices: Record<string, readonly [input: number, output: number]> = {
  'gemini-4-hydrogen': [1.5, 7.5],
};

export function profileFor(name: string, vertex?: { project: string; location: string; gemini?: boolean; model?: string }): ModelProfile {
  if (!Object.hasOwn(profileRows, name)) throw new Error(`Unknown profile: ${name}`);
  const gemini = vertex?.gemini;
  const geminiId = vertex?.model ?? 'gemini-4-hydrogen';
  if (gemini && (!/^gemini-[\w-]+$/.test(geminiId) || !Object.hasOwn(geminiPrices, geminiId))) {
    throw new Error(`Unpriced Vertex Gemini model: ${geminiId}. Add an entry to geminiPrices.`);
  }
  const [model, input, output] = gemini ? [geminiId, ...geminiPrices[geminiId]!] : profileRows[name as ProfileId];
  const provider = gemini ? 'vertex-gemini' : vertex ? 'vertex' : 'anthropic';
  const location = vertex?.location;
  const host = location === 'global' ? 'aiplatform.googleapis.com' :
    location === 'eu' || location === 'us' ? `aiplatform.${location}.rep.googleapis.com` : `${location}-aiplatform.googleapis.com`;
  const regional = vertex && location !== 'global' ? 1.1 : 1;
  const inputUsdPerMillion = input * regional;
  return { model, inputUsdPerMillion, outputUsdPerMillion: output * regional, provider,
    ...modelCachePricing(provider, model, inputUsdPerMillion),
    endpoint: vertex ? `https://${host}/v1/projects/${vertex.project}/locations/${location}/publishers/${gemini ? 'google' : 'anthropic'}/models` :
      'https://api.anthropic.com', maxOutputTokens: gemini ? 4096 : name === 'default' ? 2048 : 1024, credentialId: 'eval' };
}

const workloads = {
  current: [34_000, 4_000],
  'new-loop': [34_000, 4_000],
  bash: [26_000, 3_000],
  claude: [54_000, 6_000],
  'gemini-cli': [54_000, 6_000],
} as const;

function ceilUsd(amount: number) {
  return Math.ceil(amount * 1_000_000 - 1e-8) / 1_000_000;
}

export function trialBudget(subject: SubjectId, profile: ModelProfile, task: Task) {
  // The archive has 55 large pages. Native batching still accumulated 703k
  // input tokens on its first trial. External CLI turns may replay the pages.
  const [normalInput, normalOutput] = workloads[subject];
  const [input, output] = task.category === 'long-context'
    ? [subject === 'gemini-cli' ? 4_000_000 : 1_200_000, 60_000]
    : task.category === 'many-tools' ? [normalInput * 3, normalOutput * 2]
      : [normalInput * Math.max(1, (task.expected.minCalls ?? 1) / 3),
        normalOutput * Math.max(1, (task.expected.minCalls ?? 1) / 3)];
  if (subject !== 'new-loop') return ceilUsd(modelCost(profile, input, output));
  // Include the cold input rate and room for one maximum-size tool batch.
  // Anthropic's explicit breakpoint can bill a 1-hour cache write.
  const reserveInput = input + 32_768 + profile.maxOutputTokens + 512;
  const writePrice = profile.cacheWriteUsdPerMillion ?? profile.inputUsdPerMillion;
  return ceilUsd(priceMultiplier(profile, reserveInput) * (reserveInput * writePrice +
    (output + Math.min(4096, profile.maxOutputTokens)) * profile.outputUsdPerMillion) / 1_000_000);
}

export const judgeBudget = (profile: ModelProfile) => ceilUsd(modelCost(profile, 7500, 256));

function observedEstimate(subject: SubjectId, profile: ModelProfile, task: Task) {
  const samples = runOneCosts[subject]?.[task.category];
  const archive = task.category === 'long-context';
  const proxy = subject === 'bash' ? 'new-loop' : subject;
  const comparable = Object.values(runOneCosts[proxy] ?? {}).filter(([count]) => count >= 9);
  const exact = subject !== 'bash' && samples && !archive ? samples : undefined;
  const fallback = !archive && comparable.length ? [
    comparable.reduce((total, [count]) => total + count, 0),
    Math.max(...comparable.map(([, cost]) => cost)),
    Math.max(...comparable.map(([, , judge]) => judge)),
  ] as const : undefined;
  const nativeArchive = runOneCosts['new-loop']?.['long-context'];
  const coldArchive = archive && (subject === 'gemini-cli' || subject === 'bash') && nativeArchive
    ? [1, modelCost({ inputUsdPerMillion: 1.5, outputUsdPerMillion: 7.5 },
      runOneArchive.inputTokens, runOneArchive.outputTokens), nativeArchive[2]] as const : undefined;
  const observation = exact ?? fallback ?? (archive ? samples ?? coldArchive : undefined);
  const modelP95 = observation?.[1];
  const judgeP95 = observation?.[2];
  const workload = archive ? (coldArchive ? (subject === 'gemini-cli' ? 1.5 : 1) : 2) : task.category === 'many-tools' ? 2 : Math.max(1, (task.expected.minCalls ?? 1) / 3);
  const region = profile.inputUsdPerMillion / 1.5;
  const calibrated = modelP95 !== undefined && judgeP95 !== undefined
    ? ceilUsd((2 * workload * modelP95 + 2 * judgeP95) * region)
    : trialBudget(subject, profile, task) + judgeBudget(profile);
  return { maxUsd: calibrated, observed: {
    category: task.category,
    method: exact ? 'category p95 × 2' : fallback ? `max category p95 × 2 (${proxy} proxy)`
      : coldArchive ? 'single native archive repriced without cache × 2, scaled for the CLI'
        : observation ? 'single archive sample × 4' : 'workload reservation (no comparable observations)',
    samples: observation?.[0] ?? 0, ...(coldArchive && { archiveProxy: runOneArchive }), modelP95Usd: modelP95, judgeP95Usd: judgeP95, workload,
    ...(subject === 'bash' && samples && { failedFloor: { samples: samples[0], modelP95Usd: samples[1] } }),
  } };
}

export function estimate(subjects: readonly SubjectId[], profile: ModelProfile, tasks: readonly Task[], trials: number, profileName = 'default',
  include: (subject: SubjectId, task: Task, trial: number) => boolean = () => true) {
  const observed = profile.model === 'gemini-4-hydrogen' && profileName === 'default';
  return subjects.map((subject) => {
    const basis = new Map<string, { taskCount: number; pendingTrials: number; maxUsdPerTrial: number; observed: unknown }>();
    let micros = 0;
    let allocationMicros = 0;
    let taskCount = 0;
    for (const task of tasks) {
      const count = Array.from({ length: trials }, (_, i) => i + 1).filter((trial) => include(subject, task, trial)).length;
      if (!count) continue;
      taskCount++;
      allocationMicros += Math.round((trialBudget(subject, profile, task) + judgeBudget(profile)) * 1_000_000) * count;
      const row = observed ? observedEstimate(subject, profile, task) :
        { maxUsd: trialBudget(subject, profile, task) + judgeBudget(profile), observed: undefined };
      micros += Math.round(row.maxUsd * 1_000_000) * count;
      if (!row.observed) continue;
      const key = JSON.stringify(row.observed);
      const cell = basis.get(key) ?? { taskCount: 0, pendingTrials: 0, maxUsdPerTrial: row.maxUsd, observed: row.observed };
      cell.taskCount++;
      cell.pendingTrials += count;
      basis.set(key, cell);
    }
    return { subject, taskCount, trials, maxUsd: micros / 1_000_000, allocationUsd: allocationMicros / 1_000_000,
      ...(observed && { source: runOneSource, basis: [...basis.values()] }) };
  });
}

type Invoke = (profile: ModelProfile, request: ModelRequest, fields: Record<string, string>, signal?: AbortSignal, dispatched?: () => void, respond?: (status: number) => void) =>
  Promise<{ text: string; inputTokens: number; outputTokens: number; cacheReadTokens?: number }>;

type InvokeStream = Parameters<typeof streamModel>[2]['invoke'];

export function budgetedGateway(profileName: string, profile: ModelProfile, credential: string | (() => Promise<string>), cap: number, invoke: Invoke = invokeModel, streaming = false, streamInvoke?: InvokeStream) {
  let micros = 0;
  let denied = false;
  const limit = Math.round(cap * 1_000_000);
  const holds = new Map<string, number>();
  const model = gatewayModel(profileName, {
    gatewayUrl: 'http://eval-gateway.invalid', processToken: 'fixture', capability: () => 'fixture',
    maxOutputTokens: profile.maxOutputTokens, streaming,
    async fetch(_url, init) {
      const body = String(init?.body);
      try {
        const key = typeof credential === 'string' ? credential : await credential();
        const fields: Record<string, string> = profile.provider === 'vertex' ? { access_token: key } : { api_key: key };
        if (streaming) {
          const signal = init?.signal ?? new AbortController().signal;
          const result = await streamModel({ ...JSON.parse(body) as StreamInput, attemptLimit: 1 }, profile, {
            signal, availableMs: 60_000, secrets: [key],
            async reserve({ amount_usd, minimum_usd = amount_usd, headroom_usd = 0 }) {
              const hold = Math.min(limit - micros - Math.ceil(headroom_usd * 1_000_000), Math.ceil(amount_usd * 1_000_000));
              if (hold < Math.ceil(minimum_usd * 1_000_000)) { denied = true; return { level: 'step' }; }
              const reservation_id = randomUUID();
              holds.set(reservation_id, hold);
              micros += hold;
              return { reservation_id, amount_usd: hold / 1_000_000 };
            },
            async settle({ reservation_id, actual_usd }) {
              const hold = holds.get(reservation_id)!;
              micros += Math.ceil(actual_usd * 1_000_000) - hold;
              holds.delete(reservation_id);
            },
            invoke: (call, tick, observe, dispatched, respond) => {
              return streamInvoke ? streamInvoke(call, tick, observe, dispatched, respond) : providerModel(profile, fields, (fetcher) =>
                observeProviderFetch(fetcher, tick, observe, undefined, streamResponseLimit(profile), respond), dispatched).doStream(call);
            },
            finished() {},
          });
          return new Response(result.body, { headers: result.headers });
        }
        const result = await generateModel({
          resolve: async () => profile,
          material: async () => ({ fields, binding: { endpoint: profile.endpoint, provider: profile.provider } }),
          async reserve({ amount_usd }) {
            const hold = Math.ceil(amount_usd * 1_000_000);
            if (micros + hold > limit) { denied = true; return { level: 'step' }; }
            const reservation_id = randomUUID();
            holds.set(reservation_id, hold);
            micros += hold;
            return { reservation_id };
          },
          async settle({ reservation_id, actual_usd }) {
            micros += Math.ceil(actual_usd * 1_000_000) - holds.get(reservation_id)!;
            holds.delete(reservation_id);
          },
          invoke: (p, request, fields, dispatched, respond) => invoke(p, request, fields, init?.signal ?? undefined, dispatched, respond),
          redact: (value) => value.replaceAll(key, '[REDACTED]'),
        }, JSON.parse(body) as Parameters<typeof generateModel>[1]);
        return Response.json(result);
      } catch (error) {
        if (error instanceof HttpError) return Response.json({ error: { message: error.message } }, { status: error.status });
        return Response.json({ error: { message: 'Eval gateway failed' } }, { status: 502 });
      }
    },
  });
  return { model, cost: () => micros / 1_000_000, denied: () => denied };
}
