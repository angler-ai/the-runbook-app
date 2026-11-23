import { isCredentialPlaceholder } from '@runbook/runner-shared/credential-source';
import { deny } from './rules.ts';

const forbidden = /^(host|connection|proxy-.*|content-length|transfer-encoding|upgrade|accept-encoding|te|trailer|keep-alive|range|if-range)$/i;

export function injectHeader(headers: Headers, binding: Record<string, unknown>, value: string): void {
  if (isCredentialPlaceholder(value)) deny();
  const { header_name: name, header_format: format } = binding;
  if (typeof name !== 'string' || forbidden.test(name) || typeof format !== 'string' || format.split('{}').length !== 2) deny();
  try {
    headers.delete('range');
    headers.delete('if-range');
    headers.set(name, format.replace('{}', () => value));
  } catch {
    deny();
  }
}
