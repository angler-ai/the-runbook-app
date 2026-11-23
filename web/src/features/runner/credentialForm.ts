import type { CredentialTypeManifest } from '../../../../runner/packages/shared/src/credential-manifest';
import {
  isCredentialPlaceholder,
  isVisibleAsciiToken,
  secretManagerReference,
} from '../../../../runner/packages/shared/src/credential-source';
import {
  OAUTH_EXTRA_PARAMETERS,
  hasOAuthControlCharacters,
} from '../../../../runner/packages/shared/src/oauth-binding';
import { findSecretShapedValues } from '../../../../runner/packages/shared/src/secret-shape';
import { MAX_SECRET_BYTES } from './seal-client';
import type { Row } from './api';
import { list } from './presentation';
import { endpoint, FieldError, readText } from './runnerForms';

export function readSource(data: FormData) {
  const resource = readText(data, 'secret-resource');
  const version = readText(data, 'secret-version');
  if (!/^projects\/[a-z0-9][a-z0-9-]{0,62}\/secrets\/[A-Za-z0-9_-]{1,255}$/.test(resource))
    throw new FieldError(
      'secret-resource',
      'Secret resource name must use projects/PROJECT/secrets/SECRET.',
    );
  if (!/^(?:latest|[1-9][0-9]{0,18})$/.test(version))
    throw new FieldError(
      'secret-version',
      'Secret version must be “latest” or a positive version number.',
    );
  return secretManagerReference({ type: 'gcp-secret-manager', resource, version });
}

export function validateSecret(value: string, label: string, bearer = false, name = 'token') {
  if (!value.length) throw new FieldError(name, `${label} is required.`);
  if (new TextEncoder().encode(value).length > MAX_SECRET_BYTES)
    throw new FieldError(name, `${label} must be at most 64 KiB.`);
  if (isCredentialPlaceholder(value))
    throw new FieldError(name, `${label} cannot be a sandbox credential placeholder.`);
  if (bearer && !isVisibleAsciiToken(value))
    throw new FieldError(name, `${label} must use visible ASCII characters without spaces.`);
}

export function formDestination(
  fields: FormData,
  manifest: CredentialTypeManifest,
  baseline?: Row,
) {
  const config: Row = { ...baseline };
  for (const field of manifest.configFields) {
    if (field.name === 'test') {
      const path = readText(fields, 'test-path');
      const method = readText(fields, 'test-method');
      if (
        path &&
        (path.length > 512 ||
          !path.startsWith('/') ||
          path.startsWith('//') ||
          /[^\x21-\x7e]|[\\#]/.test(path))
      )
        throw new FieldError(
          'test-path',
          'Test path must start with one slash and use at most 512 visible ASCII characters without backslashes or a fragment.',
        );
      if (method || path) config.test = { method: method || 'GET', path: path || '/' };
      continue;
    }
    if (field.name === 'extra_parameters') {
      config.extra_parameters = Object.fromEntries(
        OAUTH_EXTRA_PARAMETERS.flatMap((name) => {
          const value = readText(fields, name);
          if (value.length > 256 || hasOAuthControlCharacters(value))
            throw new FieldError(
              name,
              `Authorization parameter ${name} must contain at most 256 characters and no control characters.`,
            );
          return value ? [[name, value]] : [];
        }),
      );
      continue;
    }
    const value = readText(fields, field.name);
    if (!value) {
      if (field.required && config[field.name] == null)
        throw new FieldError(field.name, `${field.label} is required.`);
      if (baseline && field.name === 'scopes') config.scopes = [];
      continue;
    }
    if (field.options && !field.options.includes(value))
      throw new FieldError(field.name, `${field.label} must be one of the listed options.`);
    if (field.kind === 'url')
      endpoint(value, field.label, {
        field: field.name,
        query: manifest.type === 'mcp-oauth',
        http: fields.get('provider') === 'openai-compatible',
      });
    if (field.kind === 'header-name' && !/^[!#$%&'*+.^_`|~0-9A-Za-z-]{1,64}$/.test(value))
      throw new FieldError(
        field.name,
        'Header must be a valid HTTP header name of at most 64 characters.',
      );
    if (
      field.kind === 'header-format' &&
      (value.length > 128 || value.split('{}').length !== 2 || /[^\x20-\x7e]/.test(value))
    )
      throw new FieldError(
        field.name,
        'Header value must use at most 128 printable ASCII characters and contain one {} for the key.',
      );
    if (
      field.kind === 'host-list' &&
      (!list(value).length || list(value).some((host) => !/^[a-z0-9.-]+$/i.test(host)))
    )
      throw new FieldError(
        field.name,
        'Hosts must be a comma-separated list of hostnames without a protocol, port or path.',
      );
    if (field.name === 'client_id' && (value.length > 1024 || hasOAuthControlCharacters(value)))
      throw new FieldError(
        field.name,
        'OAuth client ID must contain at most 1,024 characters and no control characters.',
      );
    const values = field.name === 'scopes' ? value.split(/[\s,]+/).filter(Boolean) : list(value);
    if (
      field.name === 'scopes' &&
      (values.length > 128 ||
        values.some((scope) => scope.length > 256 || !/^[\x21\x23-\x5b\x5d-\x7e]+$/.test(scope)))
    )
      throw new FieldError(
        field.name,
        'Scopes must use at most 128 space-separated scope names without quotes or backslashes.',
      );
    config[field.name] = field.kind.endsWith('-list') ? values : value;
  }
  if (manifest.type === 'mcp-oauth' && !config.issuer) {
    if (
      config.registration_mode === 'dynamic' &&
      (config.client_id || config.token_endpoint_auth_method)
    )
      throw new FieldError(
        'registration_mode',
        'Client ID and client authentication are decided by registration for dynamic clients.',
      );
    if (
      config.registration_mode === 'pre-registered' &&
      (!config.client_id || !config.token_endpoint_auth_method)
    )
      throw new FieldError(
        !config.client_id ? 'client_id' : 'token_endpoint_auth_method',
        'OAuth client ID and client authentication are required for an existing OAuth app.',
      );
  }
  if (manifest.secretSources && fields.get('secret-source') === 'gcp-secret-manager')
    config.secret_source = readSource(fields);
  const secret = findSecretShapedValues(config).find(
    (finding) => finding.path !== 'client_id' || finding.reason !== 'high-entropy',
  );
  if (secret)
    throw new FieldError(
      secret.path.split('.')[0] ?? '',
      `${manifest.configFields.find((field) => secret.path.startsWith(field.name))?.label ?? 'Configuration'} looks like a secret. Enter it in the secret field instead.`,
    );
  return {
    config,
    type: manifest.type,
    groups: fields.get('all-groups') === 'on' ? 'all' : fields.getAll('groups').map(String),
  };
}
