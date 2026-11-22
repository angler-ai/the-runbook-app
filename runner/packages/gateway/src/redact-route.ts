import { sandboxIdForClaim, type CapabilityVerifier } from '@runbook/runner-shared/capability';
import { MAX_TRANSCRIPT_BYTES, TRANSCRIPT_ROLES, type TranscriptRole } from '@runbook/runner-shared/contracts/internal';
import type { GrantClaims } from '@runbook/runner-shared/contracts/core';
import { HttpError, type Route } from '@runbook/runner-shared/http';
import { redactValue, type Redaction } from '@runbook/runner-shared/redact';
import type { MaterialStore } from './material.ts';

export function redactStepValue(store: MaterialStore, claims: GrantClaims, redaction: Redaction, value: unknown) {
  const released = store.forStep(claims).flatMap(({ material }) => Object.values(material.fields));
  return redactValue(value, (text) => redaction.redact(text, released));
}

const SINGLE = new Set(['system', 'input', 'output']);
const MAX_CHUNK_BYTES = 2 * 1024 * 1024;
const WINDOW_MS = 60_000;
const MAX_CHUNKS = 120;
const MAX_WINDOW_BYTES = 8 * 1024 * 1024;

interface Rate {
  expiry: number;
  start: number;
  chunks: number;
  bytes: number;
  seen: Set<string>;
}

export function stepRedactionRoute(verifier: Pick<CapabilityVerifier, 'verifyCapability'>,
  store: MaterialStore, redaction: Redaction, soleRunner?: string, now = Date.now,
  hasSandbox: (id: string) => boolean = () => true,
  append?: (record: { capability: string; role: TranscriptRole; value: unknown }, signal?: AbortSignal) => Promise<void>,
): Route {
  const rates = new Map<string, Rate>();
  return {
    method: 'POST', path: append ? '/internal/claims/transcript' : '/internal/claims/redact', callers: ['worker'],
    maxBodyBytes: (append ? MAX_TRANSCRIPT_BYTES : MAX_CHUNK_BYTES) + 8192,
    async handle({ body, runnerId, request }) {
      const input = body as { capability?: unknown; step_run_id?: unknown; role?: unknown; value?: unknown } | null;
      const role = input?.role as TranscriptRole;
      if (typeof input?.capability !== 'string' || typeof input.step_run_id !== 'string' ||
          typeof input.role !== 'string' || !TRANSCRIPT_ROLES.includes(role) ||
          !Object.hasOwn(input, 'value') ||
          ((input.role === 'system' || input.role === 'input') && typeof input.value !== 'string')) {
        throw new HttpError(422, 'redaction.invalid_request', 'invalid transcript chunk');
      }
      const bytes = Buffer.byteLength(JSON.stringify(input.value));
      if (bytes > (append ? MAX_TRANSCRIPT_BYTES : MAX_CHUNK_BYTES)) throw new HttpError(413, 'redaction.chunk_too_large', 'transcript chunk is too large');
      const claims = await verifier.verifyCapability(input.capability);
      const id = sandboxIdForClaim(claims);
      if ((runnerId ?? soleRunner) !== claims.runner_id || input.step_run_id !== claims.sub ||
          !store.active(claims) || !hasSandbox(id)) {
        throw new HttpError(403, 'redaction.claim_ended', 'redaction claim unavailable');
      }
      const at = now();
      for (const [key, rate] of rates) if (rate.expiry <= at && key !== id) rates.delete(key);
      const rate = rates.get(id) ?? { expiry: 0, start: at, chunks: 0, bytes: 0, seen: new Set<string>() };
      rate.expiry = Math.max(rate.expiry, claims.exp * 1000);
      if (rate.start + WINDOW_MS <= at) {
        rate.start = at;
        rate.chunks = 0;
        rate.bytes = 0;
      }
      if (rate.chunks >= MAX_CHUNKS || rate.bytes + bytes > MAX_WINDOW_BYTES || rate.seen.has(input.role)) {
        throw new HttpError(429, 'redaction.rate_limited', 'transcript chunk limit reached');
      }
      rate.chunks++;
      rate.bytes += bytes;
      if (SINGLE.has(input.role)) rate.seen.add(input.role);
      rates.set(id, rate);
      const value = redactStepValue(store, claims, redaction, input.value);
      if (!append) return { body: { value } };
      await append({ capability: input.capability, role, value }, request?.signal);
      return { status: 204 };
    },
  };
}
