// Shared contracts: the core server's runner routes and JWT claims
// (plan "Shared contracts", Core routes). Mirrors the Rust types in
// runbook-model; changed only by agreement between the core and runner chunks.
//
// The routes that already exist in the core (claim, heartbeat, report) are
// also in the generated OpenAPI types (../core-client.ts). The routes added by
// R0a are typed here by hand until the core's OpenAPI carries them.

export type Label = 'read' | 'write' | 'destructive';
export const LABELS: readonly Label[] = ['read', 'write', 'destructive'];

export type Delivery = 'model' | 'mcp' | 'proxy' | 'token';
export const DELIVERIES: readonly Delivery[] = ['model', 'mcp', 'proxy', 'token'];

export interface AgentCeiling {
  /** Allowed model profile names. */
  profiles: string[];
  profile_models?: Record<string, string>;
  max_cost_usd: number;
  max_tokens: number;
  timeout_s: number;
  credentials: { id: string; name: string; delivery: Delivery[]; label: Label }[];
  tools: { server_id: string; name: string; label: Label }[];
  hosts: { host: string; label: Label }[];
}

export interface AgentDirectoryEntry {
  agent_id: string;
  name: string;
  project_id: string;
  group_id: string;
  status: 'active' | 'paused' | 'archived';
  version_id: string;
  ceiling: AgentCeiling;
}

/** All decision services must advertise this read-only enforcement contract. */
export const DECISION_CONTRACT = 1;
export const DECISION_CONTRACT_HEADER = 'runbook-decision-contract';

export type GrantKind = 'capability' | 'credential';

export interface GrantRequest {
  kind: GrantKind;
  claim_id: string;
  generation: number;
  runner_id: string;
  credential_id?: string;
  credential_version?: number;
  delivery?: Delivery;
}

/** `POST /v1/step-runs/{sr}/grants` → 200. */
export interface GrantResponse {
  grant: string;
  /** Lease deadline, in epoch milliseconds. */
  expires_at: number;
}

/** JWT, typ `rb-grant+jwt`, EdDSA. */
export interface GrantClaims {
  iss: string;
  aud: `runner:${string}`;
  /** The step run ID. */
  sub: string;
  kind: GrantKind;
  claim_id: string;
  generation: number;
  runner_id: string;
  group_id: string;
  agent_id: string;
  agent_version: string;
  project_id: string;
  destructive: boolean;
  reach?: 'read';
  /** Null means the step adds no host narrowing. */
  network: string[] | null;
  /** Null means the step adds no credential narrowing. */
  connections: string[] | null;
  model_profile: string | null;
  model_id?: string;
  max_tokens: number | null;
  timeout_ms: number | null;
  step_deadline_ms: number | null;
  credential_id?: string;
  credential_version?: number;
  delivery?: Delivery;
  iat: number;
  /** The lease deadline. */
  exp: number;
  jti: string;
}

export type OrgRole = 'owner' | 'admin' | 'member';
export type ProjectRole = 'viewer' | 'operator' | 'editor' | 'admin';
export type RunnerScope = 'runner:read' | 'runner:admin';

/** JWT, typ `rb-user+jwt`, EdDSA, 60 s. */
export interface UserAssertionClaims {
  iss: string;
  aud: `runner:${string}`;
  jti: string;
  iat: number;
  exp: number;
  org_id: string;
  user_id: string;
  principal: 'human' | 'agent';
  sponsor_id?: string;
  org_role: OrgRole;
  project_roles: Record<string, ProjectRole>;
  scopes: RunnerScope[];
  /** HTTP method. */
  htm: string;
  /** Path and query. */
  htu: string;
  /** base64url SHA-256 of the body. */
  bh: string;
}

export const JWT_TYP_GRANT = 'rb-grant+jwt';
export const JWT_TYP_USER = 'rb-user+jwt';
/** The core's key IDs name their purpose (`<purpose>-…`): each `typ` verifies only with its own keys. */
export const JWT_KID_PREFIX: Readonly<Record<string, string>> = {
  [JWT_TYP_GRANT]: 'grant-',
  [JWT_TYP_USER]: 'user-',
};
/** Verifiers accept only this algorithm. */
export const JWT_ALG = 'EdDSA';
/** Clock skew verifiers allow on `exp`, `iat` and `nbf`. */
export const JWT_SKEW_SECONDS = 30;
/** User assertions live this long. */
export const USER_ASSERTION_TTL_SECONDS = 60;
/** Leases (and so grants) last this long unless renewed. */
export const LEASE_SECONDS = 600;

/** One key of `GET /.well-known/jwks.json`. */
export interface Jwk {
  kty: 'OKP';
  crv: 'Ed25519';
  x: string;
  kid: string;
  use: 'sig';
  alg: 'EdDSA';
}
export interface Jwks {
  keys: Jwk[];
}

// Registry routes.

export interface BootstrapTokenRequest {
  url: string;
}
export interface BootstrapTokenResponse {
  token: string;
}
export interface RegisterRequest {
  url: string;
  version: string;
  contract_version: number;
}
export interface RegisterResponse {
  install_id: string;
  directory_token: string;
  release_token: string;
  join_token: string;
}
export interface JoinRequest {
  name: string;
  group_id: string;
  group_name: string;
  version: string;
}
export interface JoinResponse {
  runner_id: string;
  claim_token: string;
}
export interface HeartbeatRequest {
  groups: { id: string; name: string }[];
  runners: { id: string; status: string; running: number; capacity: number }[];
}
export interface OrgRunnerStatus {
  install_id: string | null;
  status: 'online' | 'offline' | 'none';
  version: string | null;
  groups: { id: string; name: string; projects: string[] }[];
  runners: { id: string; name?: string; group_id?: string; status: string; running?: number; capacity?: number }[];
  last_heartbeat_at: string | null;
}
export interface GroupProjectsRequest {
  project_ids: string[];
}

export type WaitingReason =
  | 'agent_paused'
  | 'agent_outside_ceiling'
  | 'runner_offline'
  | `limit:${string}`;
export interface WaitingRequest {
  items: { step_run_id: string; reason: WaitingReason }[];
}

export type RunnerEventKind =
  | 'runner.budget_threshold'
  | 'runner.probe_succeeded'
  | 'runner.secret_in_response'
  | 'runner.binding_mismatch'
  | 'runner.release_denied';
export interface RunnerEventRequest {
  kind: RunnerEventKind;
  data: Record<string, unknown>;
}

export interface ReadyItem {
  kind?: 'step' | 'decision';
  step_run_id: string;
  execution_id: string;
  project_id: string;
  agent_id: string;
  ready_at: string;
  max_cost_usd: number | null;
}
export interface ReadyListing {
  items: ReadyItem[];
}

export interface GrantRevokedRequest {
  credential_id: string;
  reason: string;
}

/** Added to the existing `POST /v1/step-runs/{sr}/report` body. */
export interface ReportCost {
  model_usd: number;
  compute_usd: number;
  input_tokens: number;
  output_tokens: number;
  cached_tokens: number;
}

/** `PATCH /v1/projects/{p}` (existing) adds this field. */
export interface ProjectDefaultAgentPatch {
  default_agent: string | null;
}

/** Error codes the core returns on the runner routes. */
export type CoreRunnerErrorCode =
  | 'runner.already_registered'
  | 'runner.bad_response'
  | 'directory.name_taken'
  | 'directory.project_not_allowed'
  | 'claim.wrong_runner'
  | 'claim.outside_ceiling'
  | 'grant.claim_moved'
  | 'grant.not_allowed'
  | 'grant.lease_expired';

/** The token a core route accepts. */
export type CoreRouteAuth =
  | 'person'
  | 'bootstrap'
  | 'join'
  | 'directory'
  | 'claim'
  | 'release'
  | 'claimant'
  | 'public'
  | 'proxy';

export interface CoreRoute {
  method: 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE' | 'ANY';
  path: string;
  auth: CoreRouteAuth;
}

/** The core routes the runner phase adds or changes (R0a). */
export const CORE_RUNNER_ROUTES: readonly CoreRoute[] = [
  { method: 'POST', path: '/v1/orgs/{org}/runner/bootstrap-tokens', auth: 'person' },
  { method: 'POST', path: '/v1/runner-installs/register', auth: 'bootstrap' },
  { method: 'POST', path: '/v1/runner-installs/{ri}/runners', auth: 'join' },
  { method: 'POST', path: '/v1/runner-installs/{ri}/heartbeat', auth: 'directory' },
  { method: 'GET', path: '/v1/orgs/{org}/runner', auth: 'person' },
  { method: 'PUT', path: '/v1/orgs/{org}/runner/groups/{g}/projects', auth: 'person' },
  { method: 'PUT', path: '/v1/runner-installs/{ri}/agents/{agent_id}', auth: 'directory' },
  { method: 'DELETE', path: '/v1/runner-installs/{ri}/agents/{agent_id}', auth: 'directory' },
  { method: 'POST', path: '/v1/runner-installs/{ri}/waiting', auth: 'directory' },
  { method: 'POST', path: '/v1/runner-installs/{ri}/events', auth: 'directory' },
  { method: 'GET', path: '/v1/runner-installs/{ri}/ready', auth: 'directory' },
  { method: 'POST', path: '/v1/step-runs/{sr}/claim', auth: 'claim' },
  { method: 'POST', path: '/v1/step-runs/{sr}/grants', auth: 'release' },
  { method: 'POST', path: '/v1/step-runs/{sr}/grants/revoked', auth: 'release' },
  { method: 'GET', path: '/.well-known/jwks.json', auth: 'public' },
  { method: 'ANY', path: '/v1/runners/{ri}/proxy/{*path}', auth: 'proxy' },
  { method: 'PATCH', path: '/v1/projects/{p}', auth: 'person' },
  { method: 'POST', path: '/v1/step-runs/{sr}/report', auth: 'claimant' },
];

/** The header the core proxy forwards the user assertion in (Task R0-5). */
export const ASSERTION_HEADER = 'Runbook-Assertion';
/** The CSRF header the core proxy requires from the browser. */
export const CSRF_HEADER = 'X-Runbook-CSRF';
