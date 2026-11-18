// HOST1: how the auth service connects to Postgres, with the same meaning as
// runbook-server (crates/runbook-store-postgres/src/config.rs) for the parts
// of DATABASE_URL that postgres.js reads differently or not at all:
//
// - sslmode (libpq): disable; allow/prefer (TLS when offered, unverified; the
//   default); require (TLS, unverified unless sslrootcert is set, then as
//   verify-ca); verify-ca (the chain is checked, not the host name);
//   verify-full (chain and host name).
// - sslrootcert=<file>: PEM roots that replace the system's; `system` means
//   the system's roots, as when it is unset.
// - host=/cloudsql/<instance> (or any path): a Unix socket directory, as
//   libpq reads it; TLS never applies there. The URL may then have no host
//   (`postgres://user:pass@/db?host=/cloudsql/...`).
// - RUNBOOK_AUTH_DATABASE_POOL_SIZE: the pool's connections (default 5).
//
// These keys are taken out of the URL, so postgres.js never sends them to the
// server as startup parameters. Problems never quote the URL.
/** The node:tls options used here (the worker build has no Node types). */
interface ConnectionOptions {
  ca?: string;
  /** The name the certificate must carry (verify-full). */
  host?: string;
  rejectUnauthorized: boolean;
  checkServerIdentity?: (host: string, cert: unknown) => Error | undefined;
}

export type SslMode = 'disable' | 'prefer' | 'require' | 'verify-ca' | 'verify-full';

export interface DatabaseSettings {
  /** The URL postgres.js gets, without the keys above. */
  url: string;
  /** postgres.js options. */
  options: {
    max: number;
    host?: string;
    ssl: false | 'prefer' | 'require' | ConnectionOptions;
  };
  /** The resolved mode (tests, logs). */
  mode: SslMode;
  socket: boolean;
}

const MODES: Record<string, SslMode> = {
  disable: 'disable',
  allow: 'prefer',
  prefer: 'prefer',
  require: 'require',
  'verify-ca': 'verify-ca',
  'verify-full': 'verify-full',
};

export const DEFAULT_POOL_SIZE = 5;

/**
 * Settings for `postgres(settings.url, settings.options)`. Pushes a problem
 * (and returns null) for anything invalid; `readFile` reads sslrootcert.
 */
export function databaseSettings(
  databaseUrl: string,
  env: Record<string, string | undefined>,
  readFile: (path: string) => string,
  problems: string[],
): DatabaseSettings | null {
  const before = problems.length;
  const poolRaw = env.RUNBOOK_AUTH_DATABASE_POOL_SIZE?.trim() || String(DEFAULT_POOL_SIZE);
  const max = Number(poolRaw);
  if (!Number.isInteger(max) || max < 1 || max > 100) problems.push('RUNBOOK_AUTH_DATABASE_POOL_SIZE must be a whole number from 1 to 100');

  const scheme = /^(postgres|postgresql):\/\//.exec(databaseUrl);
  if (!scheme) {
    problems.push('DATABASE_URL must be a postgres:// URL');
    return null;
  }
  // An empty host (a socket given by ?host=) does not parse; stand one in.
  const rest = databaseUrl.slice(scheme[0].length);
  const end = rest.search(/[/?#]/);
  const authority = end === -1 ? rest : rest.slice(0, end);
  const placeholder = authority.slice(authority.lastIndexOf('@') + 1) === '';
  const parseable = placeholder ? `${scheme[0]}${authority}localhost${rest.slice(authority.length)}` : databaseUrl;
  let url: URL;
  try {
    url = new URL(parseable);
  } catch {
    problems.push('DATABASE_URL is not a valid URL');
    return null;
  }

  let mode: SslMode = 'prefer';
  let rootCert: string | null = null;
  const sslmode = url.searchParams.get('sslmode');
  if (sslmode !== null) {
    const known = MODES[sslmode];
    if (!known) problems.push('DATABASE_URL sslmode must be disable, allow, prefer, require, verify-ca or verify-full');
    else mode = known;
  }
  const sslrootcert = url.searchParams.get('sslrootcert');
  if (sslrootcert !== null) {
    if (sslrootcert === '') problems.push('DATABASE_URL sslrootcert is empty');
    else if (sslrootcert !== 'system') rootCert = sslrootcert;
  }
  const socketHost = url.searchParams.get('host');
  const socket = socketHost !== null && socketHost.startsWith('/');
  if (socketHost !== null && !socket) problems.push('DATABASE_URL host= must be a socket directory (an absolute path)');
  if (placeholder && !socket) problems.push('DATABASE_URL has no host');
  for (const key of ['sslmode', 'sslrootcert', 'host']) url.searchParams.delete(key);

  let ca: string | undefined;
  if (rootCert !== null && !socket) {
    try {
      ca = readFile(rootCert);
      if (!ca.includes('-----BEGIN CERTIFICATE-----')) throw new Error('not PEM');
    } catch {
      problems.push('DATABASE_URL sslrootcert must name a readable PEM file of CA certificates');
    }
  }
  if (problems.length > before) return null;

  let ssl: DatabaseSettings['options']['ssl'];
  if (socket || mode === 'disable') ssl = false;
  else if (mode === 'verify-full') {
    // HOST1 review: postgres.js sets no servername for an IP host, and Node
    // then checks the certificate against "localhost"; name the URL's host.
    const host = url.hostname.replace(/^\[(.*)\]$/, '$1');
    ssl = ca ? { ca, rejectUnauthorized: true, host } : { rejectUnauthorized: true, host };
  }
  else if (mode === 'verify-ca' || ca) {
    // The chain only: any host name is accepted.
    ssl = { ...(ca ? { ca } : {}), rejectUnauthorized: true, checkServerIdentity: () => undefined };
  } else ssl = mode;

  const options: DatabaseSettings['options'] = { max, ssl };
  if (socket) options.host = socketHost!;
  return { url: url.toString(), options, mode: socket ? 'disable' : mode, socket };
}
