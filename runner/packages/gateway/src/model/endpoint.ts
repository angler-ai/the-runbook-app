import { currentSpan, identifier } from '@runbook/runner-shared/telemetry';
import { definitiveProviderRejection } from './stream-fetch.ts';
import type { ReserveRequest, ReserveResponse, SettleRequest } from '@runbook/runner-shared/contracts/internal';
import { isVisibleAsciiToken } from '@runbook/runner-shared/credential-source';
import type { VertexBinding } from '@runbook/runner-shared/vertex-binding';
import { HttpError } from '@runbook/runner-shared/http';
import { estimateWorstCase, modelCost, type ModelPricing, type ModelRequest } from './pricing.ts';

export interface ModelProfile extends ModelPricing {
  credentialId: string;
  provider: 'anthropic' | 'openai-compatible' | 'vertex' | 'vertex-gemini' | 'bedrock';
  model: string;
  endpoint: string;
  vertexBinding?: VertexBinding;
  maxOutputTokens: number;
  maxResponseBytes?: number;
  servedPrices?: (ModelPricing & { model: string })[];
}

interface ModelUsage {
  text: string;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens?: number;
  cacheWriteTokens?: number;
}

interface ModelDependencies {
  resolve(capability: string, profile: string): Promise<ModelProfile | undefined>;
  material(capability: string, credentialId: string): Promise<BoundMaterial | undefined> | BoundMaterial | undefined;
  reserve(request: ReserveRequest): Promise<ReserveResponse | { level: string }>;
  invoke(profile: ModelProfile, request: ModelRequest, fields: Record<string, string>, dispatched: () => void, respond: (status: number) => void): Promise<ModelUsage>;
  settle(request: SettleRequest): Promise<void>;
  redact(value: string, values: string[]): string;
}

interface BoundMaterial {
  fields: Record<string, string>;
  binding: Record<string, unknown>;
}

export function matchesModelMaterial(profile: ModelProfile, material: BoundMaterial): boolean {
  const { binding, fields } = material;
  if (binding.endpoint !== profile.endpoint || binding.provider !== profile.provider) return false;
  if (profile.vertexBinding) {
    return Object.entries(profile.vertexBinding).every(([name, value]) => binding[name] === value) &&
      Object.keys(fields).length === 1 && isVisibleAsciiToken(fields.access_token);
  }
  return profile.provider !== 'vertex' || isVisibleAsciiToken(fields.access_token);
}

export async function generateModel(deps: ModelDependencies, input: {
  capability: string;
  profile: string;
  request: ModelRequest;
}) {
  const { capability, request } = input;
  if (typeof capability !== 'string' || typeof input.profile !== 'string' ||
      typeof request?.prompt !== 'string' ||
      (request.messages !== undefined && (!Array.isArray(request.messages) || request.messages.length > 2048 ||
        !request.messages.every((message) => message && typeof message.content === 'string' &&
          (message.role === 'system' || message.role === 'user' || message.role === 'assistant')))) || !Number.isSafeInteger(request.max_tokens) || request.max_tokens < 1) {
    throw new HttpError(400, 'model.invalid', 'invalid model request');
  }
  const profile = await deps.resolve(capability, input.profile);
  if (!profile) throw new HttpError(403, 'model.profile_denied', 'model profile is not allowed');
  if (profile.provider === 'bedrock') {
    throw new HttpError(403, 'model.attached_identity_disabled', 'attached model identity is unavailable until the vault supports it');
  }
  if (request.max_tokens > profile.maxOutputTokens) {
    throw new HttpError(400, 'model.invalid', 'output token limit exceeded');
  }
  const span = currentSpan();
  const model = identifier(profile.model);
  if (model) span?.setAttribute('gen_ai.request.model', model);
  const amount = estimateWorstCase(profile, request);
  if (!Number.isFinite(amount) || amount < 0) {
    throw new HttpError(503, 'model.pricing_invalid', 'model pricing unavailable');
  }
  const material = await deps.material(capability, profile.credentialId);
  if (!material || !matchesModelMaterial(profile, material)) {
    throw new HttpError(403, 'model.material_denied', 'model credential unavailable');
  }

  const reservation = await deps.reserve({ capability, amount_usd: amount, kind: 'model' });
  if ('level' in reservation) {
    const level = /^(step|run|agent|project|group|org)([._:-](day|month))?$/.test(reservation.level)
      ? reservation.level : 'unknown';
    throw new HttpError(402, 'model.budget_reached', `budget reached: ${level}`);
  }

  let result: ModelUsage;
  const delivery: { dispatched?: boolean; status?: number } = {};
  try {
    result = await deps.invoke(profile, request, material.fields, () => { delivery.dispatched = true; }, (code) => { delivery.status = code; });
  } catch {
    // A provider may have billed a call even if the transport returned an error.
    const pending = delivery.dispatched === true && !definitiveProviderRejection(delivery.status);
    await deps.settle({ reservation_id: reservation.reservation_id, actual_usd: pending ? amount : 0, usage: {},
      ...(pending && { usage_pending: true }) });
    throw new HttpError(502, 'model.call_failed', 'model call failed');
  }
  const cached = result.cacheReadTokens ?? 0;
  const written = result.cacheWriteTokens ?? 0;
  const usage = { input_tokens: result.inputTokens, output_tokens: result.outputTokens,
    ...(cached && { cached_tokens: cached }), ...(written && { cache_write_tokens: written }) };
  const actualUsd = modelCost(profile, result.inputTokens, result.outputTokens, cached, written);
  span?.setAttributes({
    'gen_ai.usage.input_tokens': result.inputTokens,
    'gen_ai.usage.output_tokens': result.outputTokens,
    'runbook.model.cost_usd': actualUsd,
  });
  await deps.settle({
    reservation_id: reservation.reservation_id,
    actual_usd: actualUsd,
    usage,
  });
  return { text: deps.redact(result.text, Object.values(material.fields)), usage };
}
