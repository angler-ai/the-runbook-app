// Shared contracts: routes between runner processes (plan "Runner internal
// routes"). Every call carries `Authorization: Bearer <process token>` (one
// random token per calling process and callee, from the install's generated
// env) on an internal network.
import type { components } from '../generated/core-openapi.ts';
import type { Delivery, Label } from './core.ts';

export type RunnerProcess = 'api' | 'worker' | 'sandbox-ctl' | 'vault' | 'gateway';

export interface InternalRoute {
  process: RunnerProcess;
  method: 'GET' | 'POST' | 'PUT' | 'DELETE' | 'ALL' | 'CONNECT';
  path: string;
  /** The processes allowed to call it. */
  callers: readonly (RunnerProcess | 'sandbox')[];
}

export const INTERNAL_ROUTES: readonly InternalRoute[] = [
  { process: 'worker', method: 'POST', path: '/internal/wake', callers: ['api'] },
  { process: 'vault', method: 'POST', path: '/internal/seal/{credential_id}/{field}', callers: ['api'] },
  { process: 'vault', method: 'GET', path: '/internal/ingest-key', callers: ['api'] },
  { process: 'vault', method: 'POST', path: '/internal/vertex/validate', callers: ['api'] },
  { process: 'vault', method: 'POST', path: '/internal/capability', callers: ['worker', 'sandbox-ctl'] },
  { process: 'vault', method: 'POST', path: '/internal/release', callers: ['worker'] },
  { process: 'vault', method: 'POST', path: '/internal/end-claim', callers: ['worker'] },
  { process: 'vault', method: 'POST', path: '/internal/credentials/{credential_id}/revoke', callers: ['api'] },
  { process: 'vault', method: 'POST', path: '/internal/credentials/{credential_id}/destroy', callers: ['api'] },
  { process: 'vault', method: 'POST', path: '/internal/credentials/{credential_id}/label/{operation}', callers: ['api'] },
  { process: 'gateway', method: 'DELETE', path: '/internal/material/credentials/{credential_id}', callers: ['vault'] },
  { process: 'vault', method: 'POST', path: '/internal/test/{credential_id}', callers: ['api'] },
  { process: 'vault', method: 'POST', path: '/internal/mcp/discover', callers: ['api'] },
  { process: 'gateway', method: 'POST', path: '/internal/mcp/discover', callers: ['vault'] },
  { process: 'gateway', method: 'POST', path: '/internal/material', callers: ['vault'] },
  { process: 'gateway', method: 'POST', path: '/internal/material/end-claim', callers: ['vault'] },
  { process: 'gateway', method: 'DELETE', path: '/internal/material/{release_id}', callers: ['vault'] },
  // Not in the plan's table: how the vault asks the gateway to run a
  // connection test with a test release (see the deviations file).
  { process: 'gateway', method: 'POST', path: '/internal/test', callers: ['vault'] },
  { process: 'gateway', method: 'POST', path: '/internal/claims/redact', callers: ['worker'] },
  { process: 'gateway', method: 'POST', path: '/internal/claims/transcript', callers: ['worker'] },
  { process: 'gateway', method: 'POST', path: '/model/generate', callers: ['worker'] },
  { process: 'gateway', method: 'ALL', path: '/mcp/{step_run_id}', callers: ['worker'] },
  { process: 'gateway', method: 'CONNECT', path: '*', callers: ['sandbox'] },
  { process: 'gateway', method: 'POST', path: '/internal/sandboxes', callers: ['sandbox-ctl'] },
  { process: 'gateway', method: 'POST', path: '/internal/sandboxes/{sandbox_id}/renew', callers: ['sandbox-ctl'] },
  { process: 'gateway', method: 'DELETE', path: '/internal/sandboxes/{sandbox_id}', callers: ['sandbox-ctl'] },
  { process: 'api', method: 'POST', path: '/internal/admission', callers: ['worker'] },
  { process: 'api', method: 'POST', path: '/internal/slots/{slot_id}/release', callers: ['worker'] },
  { process: 'api', method: 'POST', path: '/internal/slots/sandbox/{sandbox_id}/release', callers: ['sandbox-ctl'] },
  { process: 'api', method: 'POST', path: '/internal/slots/{slot_id}/cost', callers: ['worker'] },
  { process: 'api', method: 'POST', path: '/internal/slots/{slot_id}/heartbeat', callers: ['worker'] },
  { process: 'api', method: 'POST', path: '/internal/model/verified', callers: ['gateway'] },
  { process: 'api', method: 'POST', path: '/internal/egress/policy', callers: ['gateway'] },
  { process: 'api', method: 'POST', path: '/internal/mcp/snapshot', callers: ['gateway'] },
  { process: 'api', method: 'POST', path: '/internal/mcp/tool-calls', callers: ['gateway'] },
  { process: 'api', method: 'POST', path: '/internal/transcripts', callers: ['gateway'] },
  { process: 'api', method: 'POST', path: '/internal/ledger/reserve', callers: ['gateway', 'sandbox-ctl'] },
  { process: 'api', method: 'POST', path: '/internal/ledger/settle', callers: ['gateway', 'sandbox-ctl'] },
  { process: 'api', method: 'POST', path: '/internal/compute/rates', callers: ['sandbox-ctl'] },
  { process: 'api', method: 'POST', path: '/internal/sandboxes/{sandbox_id}/meter', callers: ['sandbox-ctl'] },
  { process: 'api', method: 'POST', path: '/internal/compute/probe-alert', callers: ['sandbox-ctl'] },
  { process: 'sandbox-ctl', method: 'POST', path: '/internal/sandboxes', callers: ['worker'] },
  { process: 'sandbox-ctl', method: 'POST', path: '/internal/sandboxes/{id}/renew', callers: ['worker'] },
  { process: 'sandbox-ctl', method: 'POST', path: '/internal/sandboxes/{id}/exec', callers: ['worker'] },
  { process: 'sandbox-ctl', method: 'GET', path: '/internal/sandboxes/{id}/files/*', callers: ['worker'] },
  { process: 'sandbox-ctl', method: 'PUT', path: '/internal/sandboxes/{id}/files/*', callers: ['worker'] },
  { process: 'sandbox-ctl', method: 'DELETE', path: '/internal/sandboxes/{id}', callers: ['worker'] },
];

export interface SlotOutcome {
  claim_id: string;
  generation: number;
  status: components['schemas']['ReportStatus'];
  idle_stop: boolean;
}

/** The header that carries a step's capability on the gateway's MCP endpoint. */
export const CAPABILITY_HEADER = 'X-Runbook-Capability';
export const MCP_ACCESS_BLOCKED = -32040;
export interface McpAccessBlocked {
  kind: 'missing_access' | 'insufficient_scope';
  release_id?: string;
  token_version?: number;
}

export interface McpDiscoveryRequest {
  url: string;
  transport: 'streamable-http' | 'sse';
  credential_id?: string;
  release_id?: string;
}

export interface EgressPolicy {
  destructive: boolean;
  layers: readonly Record<string, 'read' | 'write'>[];
  credentialLabels: Record<string, Label>;
}

export const TRANSCRIPT_ROLES = ['system', 'input', 'model', 'tool', 'output'] as const;
export type TranscriptRole = (typeof TRANSCRIPT_ROLES)[number];
export const MAX_TRANSCRIPT_BYTES = 32 * 1024;

export interface ToolCallRecord {
  capability: string;
  server_id: string;
  tool: string;
  args_redacted: Record<string, unknown>;
  args_bytes: number;
  result_bytes: number;
  duration_ms: number;
  error?: 'tool_failed' | 'tool_unavailable';
}

// vault

export interface SealRequest {
  /** HPKE blob from `sealSecret` (base64url). */
  blob: string;
}
export interface SealResponse {
  credential_id: string;
  field: string;
  version: number;
  fingerprint: string;
}

export interface IngestKeyResponse {
  ingest_key_id: string;
  /** base64url raw X25519 public key. */
  public_key: string;
  /** RFC 3339. */
  expires_at: string;
  org_id?: string;
  install_id?: string;
  /** Base64url Ed25519 signature from the vault's registered identity. */
  identity_signature?: string;
}

export interface CapabilityRequest {
  step_run_id: string;
  claim_id: string;
  generation: number;
  runner_id: string;
}
export interface CapabilityResponse {
  /** A core grant of kind `capability`. */
  capability: string;
  expires_at: string;
}

export interface ReleaseRequest {
  capability: string;
  credential_id: string;
  delivery: Delivery;
}
/** Material is pushed to the gateway, never returned. */
export interface ReleaseResponse {
  release_id: string;
  material_handle: string;
  expires_at: string;
}

export interface TestRequest {
  credential_id: string;
}
export type TestErrorClass = 'unauthorized' | 'forbidden' | 'unreachable' | 'tls' | 'unexpected-response';
export const TEST_ERROR_CLASSES: readonly TestErrorClass[] = [
  'unauthorized',
  'forbidden',
  'unreachable',
  'tls',
  'unexpected-response',
];
export interface TestResponse {
  ok: boolean;
  error_class?: TestErrorClass;
}

// gateway

/** What a release delivers: the secret fields of one credential version. */
export interface ReleaseMaterial {
  fields: Record<string, string>;
  /** Only the OAuth access token is projected into this shape. */
  mcp_oauth?: { access_token: string; token_version: number; generation: number };
}

export interface ClaimFence {
  sub: string;
  claim_id: string;
  generation: number;
  runner_id: string;
}

export interface MaterialPush {
  /** Required for live step releases. Absent only for vault connection tests. */
  claim_id?: string;
  generation?: number;
  runner_id?: string;
  release_id: string;
  step_run_id: string;
  credential_id: string;
  delivery: Delivery;
  /** The credential's destination binding (canonical form; see binding.ts). */
  binding: Record<string, unknown>;
  material: ReleaseMaterial;
  /** RFC 3339; the gateway drops the material then unless pushed again. */
  expires_at: string;
}

/** A connection test the gateway runs with a test release's material. */
export interface GatewayTestRequest {
  release_id: string;
  request: {
    method: 'GET' | 'POST' | 'HEAD';
    url: string;
    headers?: Record<string, string>;
    body?: string;
  };
}
/** The gateway reports only the status or a network failure class. */
export type GatewayTestResponse =
  | { outcome: 'response'; status: number }
  | { outcome: 'unreachable' | 'tls' };

export interface ModelGenerateRequest {
  capability: string;
  profile: string;
  request: Record<string, unknown>;
}

export interface SandboxListenerRequest {
  capability: string;
  sandbox_id: string;
  listen_host?: string;
}
export interface SandboxListenerResponse {
  proxy_url: string;
  listener_secret: string;
  ca_pem: string;
}

// api (scheduler and ledger)

export interface AdmissionRequest {
  runner_id: string;
  free_slots: number;
}
export interface AdmissionResponse {
  items: { step_run_id: string; slot_id: string; kind?: 'step' | 'decision' }[];
}
export interface ReserveRequest {
  minimum_usd?: number;
  headroom_usd?: number;
  capability: string;
  amount_usd: number;
  kind: 'model' | 'compute';
}
export interface ReserveResponse {
  amount_usd?: number;
  reservation_id: string;
}
/** 402 body. */
export interface ReserveRefused {
  level: string;
}
export interface SettleRequest {
  reservation_id: string;
  actual_usd: number;
  usage: Record<string, number>;
  usage_pending?: boolean;
}

// sandbox-ctl

export type SandboxSize = 'small' | 'medium' | 'large';
export interface CreateSandboxRequest {
  capability: string;
  size: SandboxSize;
  image: string;
}
export interface CreateSandboxResponse {
  sandbox_id: string;
}
export interface ExecRequest {
  argv: string[];
  cwd: string;
  timeout_s: number;
  stdin?: string;
  return_partial?: boolean;
}
export interface ExecResponse {
  timed_out?: boolean;
  exit_code: number;
  stdout: string;
  /** The sandbox retained bounded head and tail output. */
  truncated?: boolean;
  stdout_redacted?: boolean;
  stderr: string;
}

/** The error body every runner route returns. Messages never carry values. */
export interface ErrorBody {
  error: { code: string; message: string };
}
