import { fetchWithTrace, inSpan, setLabels } from '@runbook/runner-shared/telemetry';
import { CREDENTIAL_TYPES } from '@runbook/runner-shared/credential-types';
import type { Config } from '@runbook/runner-shared/config';
import { HttpError, type Route } from '@runbook/runner-shared/http';
import type { Redaction } from '@runbook/runner-shared/redact';
import type { CapabilityVerifier } from '../capability.ts';
import type { MaterialStore } from '../material.ts';
import { generateModel, matchesModelMaterial, type ModelProfile } from './endpoint.ts';
import { invokeModel, providerModel } from './providers.ts';

import { streamModel, type StreamInput } from './stream.ts';
import { observeProviderFetch, streamResponseLimit } from './stream-fetch.ts';

export function modelRoute(config: Config, verifier: Pick<CapabilityVerifier, 'verifyCapability'>, store: MaterialStore, redaction: Redaction, streaming = false): Route {
  async function api(path: string, body: unknown, permittedStatus = 200, timeoutMs = 10_000): Promise<Response> {
    try {
      const response = await fetchWithTrace(`${config.urls.api}${path}`, {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          authorization: `Bearer ${config.outboundTokens.api}`,
        },
        body: JSON.stringify(body),
        redirect: 'error',
        signal: AbortSignal.timeout(timeoutMs),
      });
      if (response.ok || response.status === permittedStatus) return response;
    } catch {
      // Upstream errors can carry request details.
    }
    throw new HttpError(502, 'model.api_unavailable', 'model control plane unavailable');
  }

  return {
    method: 'POST', path: streaming ? '/model/stream' : '/model/generate', maxBodyBytes: streaming ? 8 * 1024 * 1024 : undefined, callers: ['worker'],
    async handle({ body, request: inbound, runnerId }) {
      if (!body || typeof body !== 'object') throw new HttpError(400, 'model.invalid', 'invalid model request');
      const input = body as Parameters<typeof generateModel>[1];
      const claims = await verifier.verifyCapability(input.capability);
      const authenticatedRunner = runnerId ?? (config.runnerIds?.length === 1 ? config.runnerIds[0] : undefined);
      if (!authenticatedRunner || claims.runner_id !== authenticatedRunner) {
        throw new HttpError(403, 'model.runner_denied', 'worker is not authorized for this runner');
      }
      if (claims.model_profile !== null && input.profile !== claims.model_profile) {
        throw new HttpError(403, 'model.profile_denied', 'model profile is not allowed');
      }
      if (!streaming && claims.max_tokens !== null && input.request?.max_tokens > claims.max_tokens) {
        throw new HttpError(400, 'model.invalid', 'output token limit exceeded');
      }
      setLabels({ project: claims.project_id, step_run: claims.sub });
      const now = Date.now();
      const availableMs = Math.min(claims.timeout_ms ?? Infinity, claims.exp * 1000 - now,
        (claims.step_deadline_ms ?? Infinity) - now);
      if (availableMs <= 0) throw new HttpError(403, 'model.claim_ended', 'model claim unavailable');
      const timeout = AbortSignal.timeout(availableMs);
      const signal = inbound?.signal ? AbortSignal.any([timeout, inbound.signal]) : timeout;
      if (!store.active(claims)) throw new HttpError(403, 'model.claim_ended', 'model claim unavailable');
      if (streaming) {
        const streamInput = body as StreamInput;
        const response = await api('/internal/model/profile', { capability: input.capability, profile: input.profile }, 403);
        if (response.status === 403) throw new HttpError(403, 'model.profile_denied', 'model profile is not allowed');
        const profile = await response.json() as ModelProfile & { verification?: string };
        if (profile.provider === 'bedrock') {
          throw new HttpError(403, 'model.attached_identity_disabled', 'attached model identity is unavailable');
        }
        const findRelease = () => store.forStep(claims).find((candidate) =>
          candidate.delivery === 'model' && candidate.credential_id === profile.credentialId &&
          matchesModelMaterial(profile, { ...candidate.material, binding: candidate.binding }));
        const release = findRelease();
        if (!release) throw new HttpError(403, 'model.material_denied', 'model credential unavailable');
        const secrets = store.forStep(claims).flatMap(({ delivery, material }) => [
          ...Object.values(CREDENTIAL_TYPES).filter((type) => type.deliveries.includes(delivery))
            .flatMap((type) => type.secretFields.map(({ name }) => material.fields[name] ?? '')),
          ...(delivery === 'model' && material.fields.access_token ? [material.fields.access_token] : []),
        ]).filter((value) => value.length >= 8);
        return streamModel(streamInput, profile, {
          signal, availableMs, secrets,
          async reserve(request) {
            return await (await api('/internal/ledger/reserve', request, 402)).json() as
              { reservation_id: string; amount_usd?: number } | { level: string };
          },
          async settle(request) { await api('/internal/ledger/settle', { ...request, capability: input.capability }); },
          invoke(call, tick, observe, dispatched, respond) {
            call.abortSignal?.throwIfAborted();
            const current = findRelease();
            if (!current) throw new Error('model release unavailable');
            // The vault can rotate the token while the ledger reservation is in flight.
            const token = current.material.fields.access_token;
            if (token && !secrets.includes(token)) secrets.push(token);
            return providerModel(profile, current.material.fields, (fetcher) =>
              observeProviderFetch(fetcher, tick, observe, undefined, streamResponseLimit(profile), respond), dispatched).doStream(call);
          },
          finished() {
            if (profile.verification) void api('/internal/model/verified', {
              capability: input.capability, profile: input.profile, verification: profile.verification,
            }, 409, 2000).catch(() => undefined);
          },
        });
      }
      let verification: string | undefined;
      const result = await inSpan('model.call', () => generateModel({
        async resolve(capability, profile) {
          const response = await api('/internal/model/profile', { capability, profile }, 403);
          if (response.status === 403) return undefined;
          const selected = await response.json() as ModelProfile & { verification?: string };
          verification = selected.verification;
          return selected;
        },
        material(_capability, credentialId) {
          const release = store.forStep(claims).find((candidate) =>
            candidate.delivery === 'model' && candidate.credential_id === credentialId);
          return release && { fields: release.material.fields, binding: release.binding };
        },
        async reserve(request) {
          const response = await api('/internal/ledger/reserve', request, 402);
          return await response.json() as { reservation_id: string } | { level: string };
        },
        invoke(profile, request, _fields, dispatched, respond) {
          // Reservation involves network I/O. Recheck expiry/revocation before sending credentials.
          const release = store.forStep(claims).find((candidate) =>
            candidate.delivery === 'model' && candidate.credential_id === profile.credentialId &&
            matchesModelMaterial(profile, { ...candidate.material, binding: candidate.binding }));
          signal.throwIfAborted();
          if (!release) throw new Error('model release unavailable');
          const secrets = store.forStep(claims).flatMap(({ material }) => Object.values(material.fields));
          return invokeModel(profile, request, release.material.fields, signal, dispatched, respond).then((result) => ({
            ...result,
            text: redaction.redact(result.text, secrets),
          }));
        },
        async settle(request) {
          await api('/internal/ledger/settle', { ...request, capability: input.capability });
        },
        redact: redaction.redact,
      }, input));
      if (verification) {
        await api('/internal/model/verified', {
          capability: input.capability, profile: input.profile, verification,
        }, 409, 2000).catch(() => undefined);
      }
      return { body: result };
    },
  };
}
