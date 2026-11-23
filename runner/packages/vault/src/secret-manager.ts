import { isUtf8 } from 'node:buffer';
import { secretManagerReference, type SecretManagerReference } from '@runbook/runner-shared/credential-source';
import { MAX_SECRET_BYTES } from '@runbook/runner-shared/seal-client';
import { GoogleAuth } from 'google-auth-library';
import { VaultError } from './errors.ts';
import { host, httpsUrl, record } from './types/common.ts';

const endpoint = 'https://secretmanager.googleapis.com';
const maxResponseBytes = 2 * MAX_SECRET_BYTES;

type CredentialType = 'api-key' | 'mcp-bearer' | 'model-provider';
const isSupportedType = (type: unknown): type is CredentialType =>
  type === 'api-key' || type === 'mcp-bearer' || type === 'model-provider';

interface ResourcePolicy {
  resource: string;
  version?: string;
  destinations: Partial<Record<CredentialType, string[]>>;
}

export function secretManagerResources(configured: string | undefined): ResourcePolicy[] {
  try {
    const entries: unknown = JSON.parse(configured ?? '[]');
    if (!Array.isArray(entries)) throw new Error();
    return entries.map((value: unknown) => {
      const { resource, version, types, destinations, ...extra } = record(value);
      if (Object.keys(extra).length || !Array.isArray(types) || !types.length || !types.every(isSupportedType) ||
          !Array.isArray(destinations) || !destinations.length) throw new Error();
      const reference = secretManagerReference({ type: 'gcp-secret-manager', resource, version: version === undefined ? 'latest' : version });
      const allowed: ResourcePolicy['destinations'] = {};
      for (const destination of destinations as unknown[]) {
        if (typeof destination !== 'string') throw new Error();
        if (!destination.includes('://') && types.includes('api-key')) {
          (allowed['api-key'] ??= []).push(host(destination, 'destinations'));
          continue;
        }
        if (!destination.includes('://')) throw new Error();
        const allowHttp = types.includes('model-provider') && process.env.RUNNER_ALLOW_HTTP_MODEL_ENDPOINTS === 'true';
        const url = httpsUrl(destination, 'destinations', { allowHttp });
        const matching = types.filter((type) => type === 'model-provider' || (type === 'mcp-bearer' && url.startsWith('https://')));
        if (!matching.length) throw new Error();
        for (const type of matching) (allowed[type] ??= []).push(url);
      }
      if (types.some((type) => !allowed[type])) throw new Error();
      return { resource: reference.resource, ...(version !== undefined && { version: reference.version }), destinations: allowed };
    });
  } catch {
    throw new Error('RUNNER_SECRET_MANAGER_RESOURCES must be a JSON array of entries with resource, types, destinations and optional version (use [] to disable)');
  }
}

export class SecretManager {
  private readonly policies: readonly ResourcePolicy[];
  private readonly identity: Pick<GoogleAuth, 'getAccessToken'>;
  private readonly request: typeof fetch;

  constructor(
    policies: readonly ResourcePolicy[],
    identity: Pick<GoogleAuth, 'getAccessToken'> = new GoogleAuth({ scopes: 'https://www.googleapis.com/auth/cloud-platform' }),
    request: typeof fetch = fetch,
  ) {
    this.policies = policies;
    this.identity = identity;
    this.request = request;
  }

  assertAllowed(reference: SecretManagerReference, type: string, binding: Record<string, unknown>): void {
    // The installer sets this independently of IAM and credential settings.
    const policies = isSupportedType(type) ? this.policies.filter((entry) => entry.resource === reference.resource &&
      (entry.version === undefined || entry.version === reference.version) && entry.destinations[type]) : [];
    if (!isSupportedType(type) || !policies.length) {
      throw new VaultError(403, 'secret_manager.not_allowed', 'the referenced resource, version or credential type is not enabled for release');
    }
    const destinations = type === 'api-key' ? binding.hosts :
      [type === 'mcp-bearer' ? binding.url : binding.endpoint];
    const permitted = Array.isArray(destinations) && destinations.length > 0 &&
      policies.some((entry) => destinations.every((destination: unknown) =>
        typeof destination === 'string' && entry.destinations[type]?.includes(destination)));
    if (!permitted) {
      throw new VaultError(403, 'secret_manager.destination_not_allowed', 'the credential destination is not enabled for this Secret Manager reference');
    }
  }

  async access(
    reference: SecretManagerReference, type: string, binding: Record<string, unknown>,
  ): Promise<{ value: Buffer; version: string }> {
    this.assertAllowed(reference, type, binding);
    try {
      const token = await this.identity.getAccessToken();
      if (!token) throw new Error();
      // The same private googleapis.com DNS and restricted VIP as Cloud KMS.
      const response = await this.request(`${endpoint}/v1/${reference.resource}/versions/${reference.version}:access`, {
        headers: { authorization: `Bearer ${token}`, accept: 'application/json' },
        redirect: 'error',
        signal: AbortSignal.timeout(10_000),
      });
      if (!response.ok) {
        await response.body?.cancel().catch(() => undefined);
        // Secret Manager reports a disabled or destroyed version as HTTP 400.
        if ([400, 403, 404].includes(response.status)) {
          throw new VaultError(502, 'secret_manager.unavailable', 'the vault cannot access the Secret Manager version');
        }
        throw new Error();
      }
      if (!response.body) throw new Error();
      const reader = response.body.getReader();
      const chunks: Uint8Array[] = [];
      let bytes = 0;
      try {
        while (true) {
          const part = await reader.read();
          if (part.done) break;
          chunks.push(part.value);
          bytes += part.value.length;
          if (bytes > maxResponseBytes) throw new Error();
        }
        const body = Buffer.concat(chunks);
        let result: { name?: unknown; payload?: { data?: unknown } };
        try {
          result = JSON.parse(body.toString('utf8')) as typeof result;
        } finally {
          body.fill(0);
        }
        const { name, payload } = result;
        // Google can return a project number for a request using the project ID.
        const resolved = typeof name === 'string' ?
          /^projects\/[a-z0-9-]+\/secrets\/([A-Za-z0-9_-]+)\/versions\/([1-9][0-9]{0,18})$/.exec(name) : null;
        const version = resolved?.[2];
        if (!version || resolved?.[1] !== reference.resource.split('/')[3] ||
            (reference.version !== 'latest' && version !== reference.version) ||
            typeof payload?.data !== 'string') throw new Error();
        const value = Buffer.from(payload.data, 'base64');
        if (!value.length || value.length > MAX_SECRET_BYTES || !isUtf8(value) || value.toString('base64') !== payload.data) {
          value.fill(0);
          throw new VaultError(502, 'secret_manager.unavailable', 'the Secret Manager value is invalid');
        }
        return { value, version };
      } finally {
        for (const chunk of chunks) chunk.fill(0);
        await reader.cancel().catch(() => undefined);
      }
    } catch (error) {
      if (error instanceof VaultError) throw error;
      throw new VaultError(503, 'secret_manager.retry', 'the vault could not read the Secret Manager version; retry later');
    }
  }
}
