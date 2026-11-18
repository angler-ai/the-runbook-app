// Configuration, read from the environment exactly as D48 names it, plus the
// variables the Rust host passes to the child (README "Contract for AUTH2").
// Every problem is collected and reported at once, so an operator fixes a
// misconfiguration in one pass. Nothing here reads a database or the network.

export type Method = 'email' | 'google';
export const ALL_METHODS: readonly Method[] = ['email', 'google'];

/** Which entry point is loading the configuration. */
export type Runtime = 'server' | 'worker';

export type EmailConfig =
  | { transport: 'smtp'; smtpUrl: string; from: string }
  | { transport: 'capture'; captureFile: string; from: string }
  | { transport: 'binding'; from: string }
  /**
   * Cloudflare Email Service through the `send_email` binding named EMAIL
   * (Shelf's shape: a structured From). `from` is RUNBOOK_EMAIL_FROM as given.
   */
  | { transport: 'cloudflare'; from: string; sender: { name?: string; email: string } };

/** `Name <addr@host>` or a bare `addr@host`, for Cloudflare Email Service. */
export function parseSender(value: string): { name?: string; email: string } | null {
  const addr = /^[^\s<>@]+@[^\s<>@]+\.[^\s<>@]+$/;
  const named = /^\s*(.*?)\s*<([^<>]+)>\s*$/.exec(value);
  if (named) {
    const [, name, email] = named;
    if (!addr.test(email)) return null;
    return name ? { name, email } : { email };
  }
  return addr.test(value.trim()) ? { email: value.trim() } : null;
}

export interface AuthConfig {
  /** Enabled sign-in methods, in a fixed order. Empty is valid (D48). */
  methods: Method[];
  /** `RUNBOOK_ORIGIN`, e.g. `https://runbooks.example.test`. Null only with no methods. */
  origin: string | null;
  /** `RUNBOOK_AUTH_SECRET`. Null only with no methods. */
  secret: string | null;
  /** Lowercase domains; empty means any. */
  allowedDomains: string[];
  email: EmailConfig | null;
  google: { clientId: string; clientSecret: string } | null;
  /** Shared with the Rust host: it presents it to /internal/*, we present it to can-register. */
  internalSecret: string;
  /** Rust's internal `can-register` URL. Null only with no methods. */
  canRegisterUrl: string | null;
  /** `__Secure-` cookies and the Secure attribute: true on an https origin. */
  secureCookies: boolean;
}

export class ConfigError extends Error {
  readonly problems: string[];
  constructor(problems: string[]) {
    super(`runbook-auth configuration error:\n${problems.map((p) => `  - ${p}`).join('\n')}`);
    this.name = 'ConfigError';
    this.problems = problems;
  }
}

const MIN_SECRET = 32;
const DOMAIN = /^(?=.{1,253}$)([a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}$/;

const str = (env: Record<string, unknown>, key: string): string | undefined => {
  const value = env[key];
  if (typeof value !== 'string') return undefined;
  const trimmed = value.trim();
  return trimmed === '' ? undefined : trimmed;
};

/** True for http(s)://localhost, 127.0.0.0/8, [::1] and *.localhost. */
export function isLoopbackOrigin(origin: string): boolean {
  let host: string;
  try {
    host = new URL(origin).hostname.toLowerCase();
  } catch {
    return false;
  }
  if (host === 'localhost' || host.endsWith('.localhost') || host === '[::1]' || host === '::1') return true;
  return /^127(\.\d{1,3}){3}$/.test(host);
}

function parseOrigin(raw: string, problems: string[]): string | null {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    problems.push(`RUNBOOK_ORIGIN must be an absolute http(s) origin, got ${JSON.stringify(raw)}`);
    return null;
  }
  if ((url.protocol !== 'https:' && url.protocol !== 'http:') || url.origin !== raw.replace(/\/$/, '')) {
    problems.push(`RUNBOOK_ORIGIN must be an origin with no path, query or credentials (for example https://runbooks.example.test), got ${JSON.stringify(raw)}`);
    return null;
  }
  return url.origin;
}

function parseHttpUrl(key: string, raw: string, problems: string[]): string | null {
  try {
    const url = new URL(raw);
    if (url.protocol === 'http:' || url.protocol === 'https:') return url.toString();
  } catch {
    /* reported below */
  }
  problems.push(`${key} must be an http(s) URL, got ${JSON.stringify(raw)}`);
  return null;
}

/**
 * Reads the configuration. Throws {@link ConfigError} listing every problem.
 *
 * Methods: `RUNBOOK_AUTH_METHODS` (comma list) is authoritative when set, and each
 * listed method must then be fully configured. When it is unset, every method
 * whose settings are present is enabled. Half-configured settings are an error
 * either way, so a typo never silently disables sign-in.
 */
export function loadConfig(env: Record<string, unknown>, runtime: Runtime): AuthConfig {
  const problems: string[] = [];

  // --- Host contract (always required) -------------------------------------
  const internalSecret = str(env, 'RUNBOOK_AUTH_INTERNAL_SECRET');
  if (!internalSecret) problems.push('RUNBOOK_AUTH_INTERNAL_SECRET is required (the Rust host generates it per boot)');
  else if (internalSecret.length < MIN_SECRET) problems.push(`RUNBOOK_AUTH_INTERNAL_SECRET must be at least ${MIN_SECRET} characters`);

  // --- Email ----------------------------------------------------------------
  const transportRaw = str(env, 'RUNBOOK_AUTH_EMAIL_TRANSPORT');
  const smtpUrl = str(env, 'RUNBOOK_SMTP_URL');
  const from = str(env, 'RUNBOOK_SMTP_FROM');
  const captureFile = str(env, 'RUNBOOK_AUTH_CAPTURE_FILE');
  const hasBinding = runtime === 'worker' && typeof env.EMAIL === 'object' && env.EMAIL !== null;
  let email: EmailConfig | null = null;
  const emailProblems: string[] = [];
  const transport = transportRaw ?? (runtime === 'worker' ? (hasBinding ? 'binding' : undefined) : smtpUrl ? 'smtp' : undefined);
  if (transport === 'smtp') {
    if (runtime === 'worker') emailProblems.push('the Worker sends codes through its EMAIL binding; RUNBOOK_AUTH_EMAIL_TRANSPORT=smtp is server-only');
    if (!smtpUrl) emailProblems.push('RUNBOOK_SMTP_URL is required for email codes');
    else if (!/^smtps?:\/\//i.test(smtpUrl)) emailProblems.push('RUNBOOK_SMTP_URL must start with smtp:// or smtps://');
    if (!from) emailProblems.push('RUNBOOK_SMTP_FROM is required for email codes');
    if (smtpUrl && from && runtime === 'server' && /^smtps?:\/\//i.test(smtpUrl)) email = { transport: 'smtp', smtpUrl, from };
  } else if (transport === 'capture') {
    if (runtime === 'worker') emailProblems.push('RUNBOOK_AUTH_EMAIL_TRANSPORT=capture is server-only (it writes a local file)');
    if (!captureFile) emailProblems.push('RUNBOOK_AUTH_CAPTURE_FILE is required with RUNBOOK_AUTH_EMAIL_TRANSPORT=capture');
    if (captureFile && runtime === 'server') email = { transport: 'capture', captureFile, from: from ?? 'The Runbook <login@example.test>' };
  } else if (transport === 'binding') {
    if (runtime !== 'worker') emailProblems.push('RUNBOOK_AUTH_EMAIL_TRANSPORT=binding is Worker-only');
    else if (!hasBinding) emailProblems.push('RUNBOOK_AUTH_EMAIL_TRANSPORT=binding needs an EMAIL send_email binding');
    if (!from) emailProblems.push('RUNBOOK_SMTP_FROM (the From address) is required for email codes');
    if (hasBinding && from) email = { transport: 'binding', from };
  } else if (transport === 'cloudflare') {
    const fromRaw = str(env, 'RUNBOOK_EMAIL_FROM');
    const sender = fromRaw ? parseSender(fromRaw) : null;
    if (runtime !== 'worker') emailProblems.push('RUNBOOK_AUTH_EMAIL_TRANSPORT=cloudflare is Worker-only (the native edition sends through SMTP)');
    else if (!hasBinding) emailProblems.push('RUNBOOK_AUTH_EMAIL_TRANSPORT=cloudflare needs a send_email binding named EMAIL');
    if (!fromRaw) emailProblems.push('RUNBOOK_EMAIL_FROM (the sender, e.g. "The Runbook <login@example.com>") is required with RUNBOOK_AUTH_EMAIL_TRANSPORT=cloudflare');
    else if (!sender) emailProblems.push('RUNBOOK_EMAIL_FROM must be an address or "Name <address>"');
    if (hasBinding && fromRaw && sender && runtime === 'worker') email = { transport: 'cloudflare', from: fromRaw, sender };
  } else if (transport !== undefined) {
    emailProblems.push(`RUNBOOK_AUTH_EMAIL_TRANSPORT must be smtp, capture, binding or cloudflare, got ${JSON.stringify(transport)}`);
  } else if (from) {
    emailProblems.push(runtime === 'worker' ? 'RUNBOOK_SMTP_FROM is set but the Worker has no EMAIL binding' : 'RUNBOOK_SMTP_FROM is set but RUNBOOK_SMTP_URL is not');
  }

  // --- Google ---------------------------------------------------------------
  const clientId = str(env, 'RUNBOOK_GOOGLE_CLIENT_ID');
  const clientSecret = str(env, 'RUNBOOK_GOOGLE_CLIENT_SECRET');
  const googleProblems: string[] = [];
  if (Boolean(clientId) !== Boolean(clientSecret)) googleProblems.push('Google sign-in needs both RUNBOOK_GOOGLE_CLIENT_ID and RUNBOOK_GOOGLE_CLIENT_SECRET');
  const google = clientId && clientSecret ? { clientId, clientSecret } : null;

  // --- Methods ----------------------------------------------------------------
  // Empty counts as unset: docker compose passes every D48 variable, unset ones empty.
  const methodsRaw = str(env, 'RUNBOOK_AUTH_METHODS');
  let methods: Method[];
  if (methodsRaw !== undefined) {
    const listed = methodsRaw.split(',').map((m) => m.trim().toLowerCase()).filter(Boolean);
    for (const m of listed) if (!(ALL_METHODS as readonly string[]).includes(m)) problems.push(`RUNBOOK_AUTH_METHODS: unknown method ${JSON.stringify(m)} (expected email and/or google)`);
    methods = ALL_METHODS.filter((m) => listed.includes(m));
    if (methods.includes('email') && !email && emailProblems.length === 0) emailProblems.push(runtime === 'worker' ? 'RUNBOOK_AUTH_METHODS lists email but there is no EMAIL binding and RUNBOOK_SMTP_FROM' : 'RUNBOOK_AUTH_METHODS lists email but RUNBOOK_SMTP_URL and RUNBOOK_SMTP_FROM are not set');
    if (methods.includes('google') && !google && googleProblems.length === 0) googleProblems.push('RUNBOOK_AUTH_METHODS lists google but RUNBOOK_GOOGLE_CLIENT_ID and RUNBOOK_GOOGLE_CLIENT_SECRET are not set');
  } else {
    methods = ALL_METHODS.filter((m) => (m === 'email' ? email !== null : google !== null));
  }
  problems.push(...emailProblems, ...googleProblems);

  // --- Settings every method needs --------------------------------------------
  const wantsMethods = methods.length > 0 || emailProblems.length > 0 || googleProblems.length > 0;
  const secret = str(env, 'RUNBOOK_AUTH_SECRET');
  const originRaw = str(env, 'RUNBOOK_ORIGIN');
  const canRegisterRaw = str(env, 'RUNBOOK_AUTH_CAN_REGISTER_URL');
  const origin: string | null = originRaw ? parseOrigin(originRaw, problems) : null;
  const canRegisterUrl: string | null = canRegisterRaw ? parseHttpUrl('RUNBOOK_AUTH_CAN_REGISTER_URL', canRegisterRaw, problems) : null;
  if (wantsMethods) {
    if (!secret) problems.push('RUNBOOK_AUTH_SECRET is required to enable any sign-in method');
    else if (secret.length < MIN_SECRET) problems.push(`RUNBOOK_AUTH_SECRET must be at least ${MIN_SECRET} characters`);
    if (!originRaw) problems.push('RUNBOOK_ORIGIN is required to enable any sign-in method');
    if (!canRegisterRaw) problems.push('RUNBOOK_AUTH_CAN_REGISTER_URL is required to enable any sign-in method');
  }
  if (email?.transport === 'capture' && origin && !isLoopbackOrigin(origin)) {
    problems.push(`RUNBOOK_AUTH_EMAIL_TRANSPORT=capture is test-only and refuses a non-loopback RUNBOOK_ORIGIN (${origin})`);
  }

  // --- Allowed domains --------------------------------------------------------
  const allowedDomains: string[] = [];
  for (const raw of (str(env, 'RUNBOOK_AUTH_ALLOWED_DOMAINS') ?? '').split(',')) {
    const domain = raw.trim().toLowerCase().replace(/^@/, '');
    if (!domain) continue;
    if (!DOMAIN.test(domain)) problems.push(`RUNBOOK_AUTH_ALLOWED_DOMAINS: ${JSON.stringify(raw.trim())} is not a domain`);
    else if (!allowedDomains.includes(domain)) allowedDomains.push(domain);
  }

  if (problems.length > 0) throw new ConfigError(problems);
  return {
    methods,
    origin,
    secret: methods.length > 0 ? (secret as string) : null,
    allowedDomains,
    email: methods.includes('email') ? email : null,
    google: methods.includes('google') ? google : null,
    internalSecret: internalSecret as string,
    canRegisterUrl,
    secureCookies: origin?.startsWith('https:') ?? false,
  };
}

/** The domain part of a normalized email address. */
export const emailDomain = (email: string): string => email.slice(email.lastIndexOf('@') + 1).toLowerCase();

export function domainAllowed(config: Pick<AuthConfig, 'allowedDomains'>, email: string): boolean {
  return config.allowedDomains.length === 0 || config.allowedDomains.includes(emailDomain(email));
}
