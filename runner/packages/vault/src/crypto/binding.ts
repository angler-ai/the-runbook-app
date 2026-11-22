// The binding hash lives in shared (runner-api computes it too); the vault
// never trusts a stored hash and recomputes it from the config itself.
export { bindingHash, canonicalJson } from '@runbook/runner-shared/binding';
