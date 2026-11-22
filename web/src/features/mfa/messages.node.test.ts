// Every MFA error code the auth service documents reads as words in the app.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { SECURITY_CODES, securityCodeMessage } from './messages.ts';

const readme = readFileSync(new URL('../../../../auth/README.md', import.meta.url), 'utf8');

test('each documented MFA and session error code has a message', () => {
  const start = readme.indexOf('## Multi-factor authentication');
  assert.ok(start > 0, 'auth/README.md has a Multi-factor authentication section');
  const mfa = readme.slice(start).replace(/## OAuth 2\.1[\s\S]*?(?=\n### )/, '');
  const codes = new Set(mfa.match(/\bauth\.[a-z_]+/g) ?? []);
  codes.delete('auth.methods');
  assert.ok(codes.size >= 6, `found ${[...codes].join(', ')}`);
  for (const code of codes) {
    assert.ok(securityCodeMessage(code), `${code} has no message in features/mfa/messages.ts`);
  }
  for (const code of SECURITY_CODES) {
    assert.doesNotMatch(securityCodeMessage(code) ?? '', /auth\./, `${code} shows a raw code`);
  }
});
