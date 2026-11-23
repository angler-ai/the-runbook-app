// Config parsing helpers for credential types. Errors name the field, never its value.
import { VaultError } from '../errors.ts';

export type Groups = 'all' | string[];

export const invalid = (what: string) =>
  new VaultError(400, 'credential.invalid_config', `invalid credential config: ${what}`);

export function record(config: unknown): Record<string, unknown> {
  if (typeof config !== 'object' || config === null || Array.isArray(config)) throw invalid('config must be an object');
  return config as Record<string, unknown>;
}

const HOST = /^(?=.{1,253}$)([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*(:\d{1,5})?$/;

/** A lower-case host name with an optional port; no scheme, path or credentials. */
export function host(value: unknown, field: string): string {
  if (typeof value !== 'string') throw invalid(`${field} must be a host name`);
  const h = value.trim().toLowerCase().replace(/\.$/, '');
  if (!HOST.test(h)) throw invalid(`${field} must be a host name (no scheme, path or credentials)`);
  return h;
}

export function hosts(value: unknown, field: string): string[] {
  if (!Array.isArray(value) || value.length === 0) throw invalid(`${field} must list at least one host`);
  return [...new Set(value.map((v) => host(v, field)))].sort();
}

/** An https URL normalized for comparison: lower-case host, no default port, no trailing slash, no query or fragment. */
export function httpsUrl(value: unknown, field: string, { allowHttp = false } = {}): string {
  if (typeof value !== 'string') throw invalid(`${field} must be a URL`);
  let u: URL;
  try {
    u = new URL(value.trim());
  } catch {
    throw invalid(`${field} must be a URL`);
  }
  if (u.protocol !== 'https:' && !(allowHttp && u.protocol === 'http:')) throw invalid(`${field} must use https`);
  if (u.username || u.password) throw invalid(`${field} must not carry credentials`);
  if (u.search || u.hash) throw invalid(`${field} must not have a query or fragment`);
  const path = u.pathname.replace(/\/+$/, '');
  return `${u.protocol}//${u.host}${path}`;
}

export function headerName(value: unknown, field: string): string {
  if (typeof value !== 'string' || !/^[A-Za-z0-9!#$%&'*+.^_`|~-]{1,64}$/.test(value)) {
    throw invalid(`${field} must be an HTTP header name`);
  }
  return value.toLowerCase();
}

export function string(value: unknown, field: string, max = 256): string {
  if (typeof value !== 'string' || value.length === 0 || value.length > max) throw invalid(`${field} must be a string`);
  return value;
}

/** Groups in canonical form: "all", or a sorted list without duplicates. */
export function canonicalGroups(groups: unknown): Groups {
  if (groups === 'all') return 'all';
  if (Array.isArray(groups) && groups.every((g) => typeof g === 'string' && g.length > 0)) {
    return [...new Set(groups as string[])].sort();
  }
  throw invalid('groups must be "all" or a list of group IDs');
}

export function groupAllowed(groups: Groups, groupId: string): boolean {
  return groups === 'all' || groups.includes(groupId);
}

