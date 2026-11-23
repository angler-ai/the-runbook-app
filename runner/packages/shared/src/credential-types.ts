// Declarative manifests of the v1 credential types (design §7.2, §14):
// fields, which config fields make up the destination binding, delivery
// modes, the default label and what the connection test does. runner-api and
// the web app build forms and checks from these; the vault's code for each
// type (packages/vault/src/types) computes bindings and test requests and
// is checked against these manifests. Types are built-in code: no plugins.
import type { Label } from './contracts/core.ts';
import { findSecretShapedValues } from './secret-shape.ts';

import { CREDENTIAL_TYPES, type CredentialType } from './credential-manifest.ts';
export * from './credential-manifest.ts';
export * from './credential-source.ts';

/** Sharing defaults to none (design §6.1). */
export const DEFAULT_SHARING = { projects: 'none' } as const;

export function effectiveLabel(label: Label | null | undefined): Label {
  return label ?? 'destructive';
}

/**
 * Check a config before it is saved: required fields present, and nothing
 * that looks like a secret. Problems name fields, never values.
 */
export function validateCredentialConfig(type: CredentialType, config: Record<string, unknown>): string[] {
  const manifest = CREDENTIAL_TYPES[type];
  if (!manifest) return ['unknown credential type'];
  const problems: string[] = [];
  for (const field of manifest.configFields) {
    const v = config[field.name];
    if (field.required && (v === undefined || v === null || v === '' || (Array.isArray(v) && v.length === 0))) {
      problems.push(`${field.name} is required`);
    }
  }
  for (const finding of findSecretShapedValues(config)) {
    problems.push(`${finding.path} looks like a secret (${finding.reason}): put this in a secret field`);
  }
  return problems;
}
