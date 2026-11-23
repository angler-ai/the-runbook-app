// Environment loading for every runner process. `loadConfig(role)` checks
// everything at once and throws one ConfigError naming every problem. Values
// of secret variables (tokens, database URLs) never appear in a problem.
import type { RunnerProcess } from './contracts/internal.ts';

export function validJwksMaxAge(seconds: number): boolean {
  return Number.isSafeInteger(seconds) && seconds >= 1 && seconds <= 86400;
}
export const JWKS_MAX_AGE_ERROR = 'RUNNER_JWKS_MAX_AGE_SECONDS must be an integer from 1 to 86400';

export type RunnerRole = RunnerProcess;
export const RUNNER_ROLES: readonly RunnerRole[] = ['api', 'worker', 'sandbox-ctl', 'vault', 'gateway'];

export type KekProvider = 'local' | 'gcp-kms';
export const KEK_PROVIDERS: readonly KekProvider[] = ['local', 'gcp-kms'];
/** Planned in the design (§7.3) but not built until R3. */
const LATER_KEK_PROVIDERS = ['aws-kms', 'openbao-transit'];

export type KekConfig = { provider: 'local'; file: string } | { provider: 'gcp-kms'; key: string };

export interface Config {
  role: RunnerRole;
  listen: { host: string; port: number };
  internalListen?: { host: string; port: number };
  /** The org this control plane serves (api, vault). */
  orgId?: string;
  coreUrl?: string;
  vaultCoreUrl?: string;
  /** The core token this process presents: directory (api), claim (worker), release (vault). */
  coreToken?: string;
  metricsToken?: string;
  coreWebhookSecret?: string;
  /** This process's own database role (api, worker, vault). */
  databaseUrl?: string;
  /** The scheduler's database role (api only; the scheduler runs in api in v1). */
  schedulerDatabaseUrl?: string;
  runnerId?: string;
  runnerIds?: string[];
  workerTokens?: Record<string, string>;
  groupId?: string;
  /** Base URLs of the processes this one calls. */
  urls: Partial<Record<RunnerProcess, string>>;
  /** Tokens this process accepts, by calling process. */
  inboundTokens: Partial<Record<RunnerProcess, string>>;
  /** Tokens this process presents, by called process. */
  outboundTokens: Partial<Record<RunnerProcess, string>>;
  kek?: KekConfig;
  dockerSocket?: string;
}

export class ConfigError extends Error {
  readonly problems: string[];
  constructor(role: string, problems: string[]) {
    super(`runner ${role}: invalid configuration:\n- ${problems.join('\n- ')}`);
    this.name = 'ConfigError';
    this.problems = problems;
  }
}

const DEFAULT_PORTS: Record<RunnerRole, number> = {
  api: 8090,
  vault: 8091,
  gateway: 8092,
  'sandbox-ctl': 8093,
  worker: 8094,
};

const TOKEN_NAME: Record<RunnerProcess, string> = {
  api: 'API',
  worker: 'WORKER',
  'sandbox-ctl': 'SANDBOXCTL',
  vault: 'VAULT',
  gateway: 'GATEWAY',
};
const URL_NAME: Record<RunnerProcess, string> = {
  api: 'API_URL',
  worker: 'WORKER_URL',
  'sandbox-ctl': 'SANDBOX_CTL_URL',
  vault: 'VAULT_URL',
  gateway: 'GATEWAY_URL',
};

/** `RUNNER_TOKEN_<CALLER>_<CALLEE>`. */
export function tokenVar(caller: RunnerProcess, callee: RunnerProcess): string {
  return `RUNNER_TOKEN_${TOKEN_NAME[caller]}_${TOKEN_NAME[callee]}`;
}

interface RoleSpec {
  /** Processes that call this one (its inbound tokens). */
  callers: RunnerProcess[];
  /** Processes this one calls (outbound tokens and URLs). */
  callees: RunnerProcess[];
  core?: { token: string };
  orgId?: boolean;
  database?: boolean;
  schedulerDatabase?: boolean;
  runner?: boolean;
  kek?: boolean;
  docker?: boolean;
}

/** Least privilege by process (design §7.9). */
const ROLES: Record<RunnerRole, RoleSpec> = {
  api: {
    callers: ['gateway', 'sandbox-ctl'],
    callees: ['vault'],
    core: { token: 'CORE_DIRECTORY_TOKEN' },
    orgId: true,
    database: true,
    schedulerDatabase: true,
  },
  worker: {
    callers: ['api'],
    callees: ['vault', 'api', 'gateway', 'sandbox-ctl'],
    core: { token: 'CORE_CLAIM_TOKEN' },
    database: true,
    runner: true,
  },
  'sandbox-ctl': {
    callers: ['worker'],
    callees: ['gateway', 'api', 'vault'],
    core: { token: '' },
    docker: true,
  },
  vault: {
    callers: ['api', 'worker', 'sandbox-ctl'],
    callees: ['gateway'],
    core: { token: 'CORE_RELEASE_TOKEN' },
    orgId: true,
    database: true,
    kek: true,
  },
  gateway: {
    callers: ['vault', 'worker', 'sandbox-ctl'],
    callees: ['api'],
    core: { token: '' },
  },
};

const MIN_TOKEN_LENGTH = 32;

export function loadConfig(role: RunnerRole, env: Record<string, string | undefined> = process.env): Config {
  const problems: string[] = [];
  if (!validJwksMaxAge(Number(env.RUNNER_JWKS_MAX_AGE_SECONDS ?? 3600))) problems.push(JWKS_MAX_AGE_ERROR);
  const spec = ROLES[role];
  if (!spec) throw new ConfigError(String(role), [`unknown role (one of ${RUNNER_ROLES.join(', ')})`]);

  const get = (name: string): string | undefined => {
    const v = env[name];
    return v === undefined || v.trim() === '' ? undefined : v.trim();
  };
  const need = (name: string): string | undefined => {
    const v = get(name);
    if (v === undefined) problems.push(`${name} is required`);
    return v;
  };
  const forbid = (name: string, why: string) => {
    if (get(name) !== undefined) problems.push(`${name} must not be set for the ${role} (${why})`);
  };
  const flag = (name: string): boolean => {
    const v = get(name);
    if (v === undefined || v === 'false') return false;
    if (v === 'true') return true;
    problems.push(`${name} must be true or false`);
    return false;
  };
  const url = (name: string, required: boolean, schemes = ['https:']): string | undefined => {
    const v = required ? need(name) : get(name);
    if (v === undefined) return undefined;
    try {
      const u = new URL(v);
      if (!schemes.includes(u.protocol)) {
        problems.push(`${name} must use ${schemes.join(' or ')}`);
        return undefined;
      }
      if (u.username || u.password) {
        if (!schemes.includes('postgres:')) problems.push(`${name} must not carry credentials`);
      }
    } catch {
      problems.push(`${name} is not a valid URL`);
      return undefined;
    }
    return v.replace(/\/+$/, '');
  };
  const secret = (name: string): string | undefined => {
    const v = need(name);
    if (v !== undefined && v.length < MIN_TOKEN_LENGTH) {
      problems.push(`${name} must be at least ${MIN_TOKEN_LENGTH} characters`);
      return undefined;
    }
    return v;
  };

  const declared = get('RUNNER_ROLE');
  if (declared === undefined) problems.push('RUNNER_ROLE is required');
  else if (declared !== role) problems.push(`RUNNER_ROLE is "${declared}" but this process is the ${role}`);

  function listenAddress(name: string, defaultPort: number) {
    const value = get(name);
    if (value === undefined) return { host: '0.0.0.0', port: defaultPort };
    const match = /^(.*):(\d{1,5})$/.exec(value);
    const port = match ? Number(match[2]) : NaN;
    if (!match || !(port > 0 && port < 65536)) {
      problems.push(`${name} must be host:port`);
      return { host: '0.0.0.0', port: defaultPort };
    }
    return { host: match[1] || '0.0.0.0', port };
  }
  const listen = listenAddress('RUNNER_LISTEN', DEFAULT_PORTS[role]);
  const config: Config = { role, listen, urls: {}, inboundTokens: {}, outboundTokens: {} };

  // Trust roots are https. Plain http is a local-development opt-in: one flag
  // for the internal URLs, a separate one for the core.
  const httpOk = ['http:', 'https:'];
  const internalSchemes = flag('RUNNER_INSECURE_INTERNAL_HTTP') ? httpOk : ['https:'];
  const coreSchemes = flag('RUNNER_INSECURE_CORE_HTTP') ? httpOk : ['https:'];

  if (role === 'api') {
    config.internalListen = listenAddress('RUNNER_INTERNAL_LISTEN', 8094);
    if (config.internalListen.port === listen.port) problems.push('RUNNER_INTERNAL_LISTEN must use a separate port');
    config.metricsToken = secret('RUNNER_METRICS_TOKEN');
    const webhookSecret = get('RUNNER_CORE_WEBHOOK_SECRET');
    if (webhookSecret) config.coreWebhookSecret = secret('RUNNER_CORE_WEBHOOK_SECRET');
  }
  if (spec.orgId) config.orgId = need('RUNNER_ORG_ID');
  config.coreUrl = url('CORE_URL', true, coreSchemes);
  if (role === 'vault') config.vaultCoreUrl = url('RUNNER_VAULT_CORE_URL', false, internalSchemes);
  else forbid('RUNNER_VAULT_CORE_URL', 'this private core link belongs to the vault');
  if (spec.core?.token) config.coreToken = secret(spec.core.token);
  for (const t of ['CORE_DIRECTORY_TOKEN', 'CORE_CLAIM_TOKEN', 'CORE_RELEASE_TOKEN']) {
    if (t !== spec.core?.token) forbid(t, 'each process holds only its own core token');
  }

  const pg = ['postgres:', 'postgresql:'];
  if (spec.database && !(role === 'worker' && env.RUNNER_SANDBOX_BACKEND === 'kubernetes')) config.databaseUrl = url('RUNNER_DATABASE_URL', true, pg);
  else forbid('RUNNER_DATABASE_URL', 'it has no database access');
  if (spec.schedulerDatabase) config.schedulerDatabaseUrl = url('RUNNER_SCHEDULER_DATABASE_URL', true, pg);
  else forbid('RUNNER_SCHEDULER_DATABASE_URL', 'only runner-api runs the scheduler');

  if (role === 'api' || role === 'gateway' || role === 'sandbox-ctl' || role === 'vault') {
    const ids = need('RUNNER_ALLOWED_RUNNER_IDS')?.split(',').map((id) => id.trim());
    if (ids?.some((id) => !/^[A-Za-z0-9_-]+$/.test(id))) {
      problems.push('RUNNER_ALLOWED_RUNNER_IDS must be a comma-separated list of runner IDs');
    }
    config.runnerIds = ids;
  }
  if (spec.runner) {
    config.runnerId = need('RUNNER_ID');
    config.groupId = need('RUNNER_GROUP_ID');
  }

  const workerMapName = role === 'api' ? 'RUNNER_WORKER_API_TOKENS'
    : role === 'vault' ? 'RUNNER_WORKER_VAULT_TOKENS' : 'RUNNER_WORKER_GATEWAY_TOKENS';
  const usesWorkerMap = role === 'api' || ((role === 'gateway' || role === 'vault') &&
    (Boolean(get(workerMapName)) || (config.runnerIds?.length ?? 0) > 1));
  for (const caller of spec.callers) {
    if (caller === 'worker' && usesWorkerMap) continue;
    const v = secret(tokenVar(caller, role));
    if (v) config.inboundTokens[caller] = v;
  }
  if (usesWorkerMap) {
    const name = workerMapName;
    const raw = need(name);
    if (raw) {
      try {
        const tokens = JSON.parse(raw) as Record<string, unknown>;
        if (!tokens || typeof tokens !== 'object' || Array.isArray(tokens)) throw new Error();
        const entries = Object.entries(tokens);
        const values = Object.values(tokens);
        if (!entries.length || entries.some(([id, value]) => !config.runnerIds?.includes(id) ||
            typeof value !== 'string' || value.length < MIN_TOKEN_LENGTH || !/^\S+$/.test(value)) ||
            config.runnerIds?.some((id) => !Object.hasOwn(tokens, id)) ||
            new Set([...values, ...Object.values(config.inboundTokens)]).size !== values.length + Object.keys(config.inboundTokens).length) {
          throw new Error();
        }
        config.workerTokens = tokens as Record<string, string>;
      } catch {
        problems.push(`${name} must map each installed runner ID to a unique token of at least 32 characters`);
      }
    }
  }
  for (const callee of spec.callees) {
    const v = secret(tokenVar(role, callee));
    if (v) config.outboundTokens[callee] = v;
    const u = url(URL_NAME[callee], true, internalSchemes);
    if (u) config.urls[callee] = u;
  }

  const kekVars = ['RUNNER_KEK_PROVIDER', 'RUNNER_KEK_LOCAL_FILE', 'RUNNER_KEK_GCP_KEY'];
  if (spec.kek) {
    const provider = need('RUNNER_KEK_PROVIDER');
    if (provider === 'local') {
      const file = need('RUNNER_KEK_LOCAL_FILE');
      if (file) config.kek = { provider, file };
    } else if (provider === 'gcp-kms') {
      const key = need('RUNNER_KEK_GCP_KEY');
      if (key && !/^projects\/[^/]+\/locations\/[^/]+\/keyRings\/[^/]+\/cryptoKeys\/[^/]+$/.test(key)) {
        problems.push('RUNNER_KEK_GCP_KEY must be projects/…/locations/…/keyRings/…/cryptoKeys/…');
      } else if (key) config.kek = { provider, key };
    } else if (provider !== undefined && LATER_KEK_PROVIDERS.includes(provider)) {
      problems.push(`RUNNER_KEK_PROVIDER "${provider}" is not supported in this release (use ${KEK_PROVIDERS.join(' or ')})`);
    } else if (provider !== undefined) {
      problems.push(
        `RUNNER_KEK_PROVIDER "${provider.slice(0, 32)}" is unknown (one of ${KEK_PROVIDERS.join(', ')})`,
      );
    }
  } else {
    for (const v of kekVars) forbid(v, 'only the vault holds key-encryption keys');
  }

  if (spec.docker) config.dockerSocket = get('RUNNER_DOCKER_SOCKET') ?? '/var/run/docker.sock';
  else forbid('RUNNER_DOCKER_SOCKET', 'only sandbox-ctl owns the container runtime');

  if (problems.length > 0) throw new ConfigError(role, problems);
  return config;
}

/** Narrow an optional field that `loadConfig` already checked for this role. */
export function loadInstallId(env: Record<string, string | undefined> = process.env): string {
  const id = env.RUNNER_INSTALL_ID;
  if (!id || !/^[A-Za-z0-9_-]+$/.test(id)) throw new Error('RUNNER_INSTALL_ID is required');
  return id;
}

export function required<T>(value: T | undefined, name: string): T {
  if (value === undefined) throw new Error(`configuration is missing ${name}`);
  return value;
}
