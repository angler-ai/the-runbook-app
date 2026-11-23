/** A non-secret reference confirmed under the same seal as a stored value. */
export interface SecretManagerReference {
  type: 'gcp-secret-manager';
  resource: string;
  version: string;
}

export function secretManagerReference(value: unknown): SecretManagerReference {
  const reference = value as { type?: unknown; resource?: unknown; version?: unknown } | null | undefined;
  const providedVersion = reference?.version;
  const version = typeof providedVersion === 'number' && Number.isSafeInteger(providedVersion) ?
    String(providedVersion) : providedVersion;
  if (!reference || Object.keys(reference).length !== 3 || reference.type !== 'gcp-secret-manager' ||
      typeof reference.resource !== 'string' ||
      !/^projects\/[a-z0-9][a-z0-9-]{0,62}\/secrets\/[A-Za-z0-9_-]{1,255}$/.test(reference.resource) ||
      typeof version !== 'string' || !/^(?:latest|[1-9][0-9]{0,18})$/.test(version)) {
    throw new Error('Invalid Secret Manager reference');
  }
  return { type: reference.type, resource: reference.resource, version };
}

export interface OAuthDiscoveryCandidate {
  config: Record<string, unknown>;
  groups: 'all' | string[];
  confirmed: false;
  registration_pending?: true;
}

export const CREDENTIAL_ENV = {
  name_pattern: '^[A-Z_][A-Z0-9_]*$',
  max_name_length: 128,
  reserved_names: [
    'PATH', 'HOME', 'SHELL', 'USER', 'LOGNAME', 'PWD', 'OLDPWD', 'IFS', 'CDPATH', 'ENV', 'SHELLOPTS',
    'PS0', 'PS1', 'PS2', 'PS3', 'PS4', 'PS5', 'PS6', 'PS7', 'PS8', 'PS9',
    'HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'NO_PROXY', 'FTP_PROXY',
    'PYTHONPATH', 'PYTHONHOME', 'PERL5LIB', 'RUBYLIB', 'RUBYOPT', 'CURL_CA_BUNDLE', 'REQUESTS_CA_BUNDLE', 'PIP_CERT', 'GCONV_PATH', 'LOCPATH', 'TMPDIR',
  ],
  reserved_prefixes: [
    'BASH', 'NSS', 'LD_', 'DYLD_', 'SSL_CERT_', 'RUNBOOK_', 'RUNNER_', 'RB_', 'KUBERNETES_', 'NODE_', 'PYTHON_',
    'PERL_', 'RUBY_', 'BUNDLE_', 'GIT_', 'CLOUDSDK_', 'GOOGLE_APPLICATION_',
  ],
  required_delivery: 'proxy',
} as const;

export const CREDENTIAL_PLACEHOLDER_PREFIX = 'rb-placeholder-';

export function isCredentialPlaceholder(value: string): boolean {
  return value.trim().toLowerCase().startsWith(CREDENTIAL_PLACEHOLDER_PREFIX);
}

export function isVisibleAsciiToken(value: unknown): value is string {
  return typeof value === 'string' && value.length > 0 && !/[^\x21-\x7e]/.test(value);
}
