import type { Delivery, Label } from './contracts/core.ts';

export type CredentialType = 'api-key' | 'model-provider' | 'mcp-bearer';

export interface ConfigFieldSpec {
  name: string;
  label: string;
  kind: 'host-list' | 'header-name' | 'header-format' | 'url' | 'enum' | 'path' | 'object' | 'string-list';
  required: boolean;
  options?: readonly string[];
  help?: string;
}

export interface CredentialTypeManifest {
  type: CredentialType;
  title: string;
  description: string;
  configFields: readonly ConfigFieldSpec[];
  secretFields: readonly { name: string; label: string }[];
  /** Config fields that are part of the destination binding (with the allowed groups). */
  bindingFields: readonly string[];
  deliveries: readonly Delivery[];
  /** The safest mode the type supports (design §7.6). */
  defaultDelivery: Delivery;
  /** Unlabeled credentials are destructive (design §6.5). */
  defaultLabel: Label;
  /** What "Test" does, in words for the UI. */
  test: string;
}

export const MODEL_PROVIDERS = ['anthropic', 'openai', 'google', 'vertex-gemini', 'azure-openai', 'openai-compatible'] as const;

export const CREDENTIAL_TYPES: Record<CredentialType, CredentialTypeManifest> = {
  'api-key': {
    type: 'api-key',
    title: 'API key',
    description: 'A key the gateway sets as a declared header on requests to the bound hosts.',
    configFields: [
      { name: 'hosts', label: 'Hosts', kind: 'host-list', required: true, help: 'Host names only, e.g. api.datadoghq.com' },
      { name: 'header_name', label: 'Header', kind: 'header-name', required: true, help: 'e.g. Authorization or DD-API-KEY' },
      { name: 'header_format', label: 'Header value', kind: 'header-format', required: true, help: '{} is replaced by the key, e.g. Bearer {}' },
      { name: 'test', label: 'Test request', kind: 'object', required: false, help: '{method: GET|HEAD, path}' },
    ],
    secretFields: [{ name: 'key', label: 'Key' }],
    bindingFields: ['hosts', 'header_name', 'header_format', 'test'],
    deliveries: ['proxy'],
    defaultDelivery: 'proxy',
    defaultLabel: 'destructive',
    test: 'Sends the test request to the first host with the key set.',
  },
  'model-provider': {
    type: 'model-provider',
    title: 'Model provider key',
    description: "A model provider's API key, added by the gateway's model endpoint.",
    configFields: [
      { name: 'provider', label: 'Provider', kind: 'enum', required: true, options: MODEL_PROVIDERS },
      { name: 'endpoint', label: 'Endpoint', kind: 'url', required: true },
      { name: 'models', label: 'Models', kind: 'string-list', required: false },
    ],
    secretFields: [{ name: 'api_key', label: 'API key' }],
    bindingFields: ['provider', 'endpoint'],
    deliveries: ['model'],
    defaultDelivery: 'model',
    defaultLabel: 'read',
    test: "Lists the provider's models.",
  },
  'mcp-bearer': {
    type: 'mcp-bearer',
    title: 'MCP bearer token',
    description: 'A bearer token the gateway adds when it calls the MCP server.',
    configFields: [{ name: 'url', label: 'MCP server URL', kind: 'url', required: true }],
    secretFields: [{ name: 'token', label: 'Token' }],
    bindingFields: ['url'],
    deliveries: ['mcp'],
    defaultDelivery: 'mcp',
    defaultLabel: 'destructive',
    test: 'Sends an MCP initialize request.',
  },
};
