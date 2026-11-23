// `api-key` (design §7.2): one stored key, set as a declared header on
// requests to the bound hosts (`proxy` delivery). The binding is the hosts,
// the header name and its format (`Bearer {}`), the test request (method and
// path, sent to the first host) and the allowed groups. The test request is
// in the binding because Test sends the key there: changing it means
// entering the secret again.
import { VaultError } from '../errors.ts';
import type { CredentialTypeImpl } from './index.ts';
import { canonicalGroups, type Groups, headerName, hosts, invalid, record, string } from './common.ts';

interface TestRequestSpec {
  method: 'GET' | 'HEAD';
  path: string;
}

/** A path on the host: starts with one `/`, visible ASCII only, no fragment. */
const TEST_PATH = /^\/(?!\/)[\x21-\x22\x24-\x7e]*$/;

function testSpec(value: unknown): TestRequestSpec {
  if (value === undefined || value === null) return { method: 'GET', path: '/' };
  const t = record(value);
  const method = t.method ?? 'GET';
  if (method !== 'GET' && method !== 'HEAD') throw invalid('test.method must be GET or HEAD');
  const path = t.path ?? '/';
  if (typeof path !== 'string' || path.length > 512 || !TEST_PATH.test(path)) {
    throw invalid('test.path must be a path on the host (starting with /)');
  }
  return { method, path };
}

export const apiKey: CredentialTypeImpl = {
  type: 'api-key',
  secretFields: ['key'],
  deliveries: ['proxy'],
  binding(config: unknown, groups: Groups) {
    const c = record(config);
    const format = string(c.header_format, 'header_format', 128);
    if (format.split('{}').length !== 2) throw invalid('header_format must contain {} exactly once');
    if (/[\r\n]/.test(format)) throw invalid('header_format must be one line');
    return {
      hosts: hosts(c.hosts, 'hosts'),
      header_name: headerName(c.header_name, 'header_name'),
      header_format: format,
      test: testSpec(c.test),
      groups: canonicalGroups(groups),
    };
  },
  testRequest(binding, config) {
    // Only the bound test request is ever sent; a config that asks for
    // another one (the binding was not recomputed from it) is refused.
    const bound = binding.test as TestRequestSpec | undefined;
    const asked = testSpec(record(config).test);
    if (!bound || bound.method !== asked.method || bound.path !== asked.path) {
      throw new VaultError(409, 'vault.binding_mismatch', 'the credential destination changed; enter the secret again');
    }
    const first = (binding.hosts as string[])[0];
    return { method: bound.method, url: `https://${first}${bound.path}` };
  },
};
