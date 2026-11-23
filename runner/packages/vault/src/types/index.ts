// Credential types the vault knows (design §7.2): built-in code, no plugin
// loading. Each type turns a credential's config into its destination
// binding, says which fields are secret and how material is delivered, and
// builds the connection test request (sent by the gateway, never the vault).
import { secretManagerReference } from '@runbook/runner-shared/credential-source';
import { oauthBinding } from '@runbook/runner-shared/oauth-binding';
import type { Delivery } from '@runbook/runner-shared/contracts/core';
import type { GatewayTestRequest, ReleaseMaterial } from '@runbook/runner-shared/contracts/internal';
import { VaultError } from '../errors.ts';
import { apiKey } from './api-key.ts';
import { mcpBearer } from './mcp-bearer.ts';
import { modelProvider } from './model-provider.ts';
import { canonicalGroups, record, type Groups } from './common.ts';

export { canonicalGroups, groupAllowed, type Groups } from './common.ts';

export interface CredentialTypeImpl {
  type: string;
  secretFields: readonly string[];
  deliveries: readonly Delivery[];
  /** The canonical destination binding, including the groups it may go to. Throws on invalid config. */
  binding(config: unknown, groups: Groups): Record<string, unknown>;
  /** The test request: always to a bound destination, with the material applied by the gateway. */
  testRequest(binding: Record<string, unknown>, config: unknown): GatewayTestRequest['request'];
}

const mcpOAuth: CredentialTypeImpl = {
  ...mcpBearer,
  type: 'mcp-oauth',
  secretFields: ['client_secret', 'access_token', 'refresh_token', 'oauth_pending'],
  binding: (config, groups) => oauthBinding(record(config), canonicalGroups(groups)),
};

const TYPES = new Map<string, CredentialTypeImpl>([apiKey, modelProvider, mcpBearer, mcpOAuth].map((t) => [t.type, t]));

export function registerType(impl: CredentialTypeImpl): void {
  TYPES.set(impl.type, impl);
}

export function typeImpl(type: string): CredentialTypeImpl {
  const impl = TYPES.get(type);
  if (!impl) throw new VaultError(400, 'credential.unknown_type', 'the credential type is not supported');
  return impl;
}

export function credentialBinding(type: string, config: unknown, groups: Groups): Record<string, unknown> {
  const binding = typeImpl(type).binding(config, groups);
  const source = record(config).secret_source;
  if (source === undefined) return binding;
  if (type !== 'api-key' && type !== 'model-provider' && type !== 'mcp-bearer') {
    throw new VaultError(400, 'credential.invalid', 'Secret Manager is unavailable for this credential type');
  }
  return { ...binding, secret_source: secretManagerReference(source) };
}

/** Material for a release: the secret fields, by name. */
export function material(fields: Record<string, string>): ReleaseMaterial {
  return { fields };
}
