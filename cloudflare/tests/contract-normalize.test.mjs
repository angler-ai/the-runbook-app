// CF9: the contract suite's normalizer (contract/normalize.mjs) hides only
// what legitimately differs between two runs (IDs, secrets, times, origins)
// and keeps everything the contract fixes: field names, status values,
// codes, counts and order.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { normalize, normalizeText } from '../contract/normalize.mjs';

test('IDs, secrets, times and origins become placeholders', () => {
  const origins = [['http://127.0.0.1:5000', '<origin>']];
  assert.deepEqual(
    normalize(
      {
        id: 'exe_01JABCDEFGHJKMNPQRSTVWXYZ0',
        secret: 'trb_pat_abcdefghijklmnopqrstuvwxyz012345',
        hook: 'whsec_abcdefghijklmnopqrstuvwxyz0123',
        created_at_ms: 1_794_000_000_000,
        expires_at: '2026-11-12T10:00:00Z',
        url: 'http://127.0.0.1:5000/u/exe_01JABCDEFGHJKMNPQRSTVWXYZ0/x?expires=1794000000000&sig=abcDEF-_1',
      },
      origins,
    ),
    {
      created_at_ms: '<ms>',
      expires_at: '<time>',
      hook: '<secret>',
      id: '<exe>',
      secret: '<secret>',
      url: '<origin>/u/<exe>/x?expires=<ms>&sig=<sig>',
    },
  );
  assert.equal(normalizeText('archived 2026-01-01 at 1794000000000'), 'archived <date> at <ms>');
  // Markdown escapes the underscore of an ID.
  assert.equal(normalizeText('Called by: exe\\_01JABCDEFGHJKMNPQRSTVWXYZ0 (step db)'), 'Called by: <exe> (step db)');
});

test('contract facts stay: status values, codes, counts and order', () => {
  const value = { status: 'waiting', code: 'step.not_claimed', revision: 2, items: [{ n: 2 }, { n: 1 }] };
  assert.deepEqual(normalize(value), { code: 'step.not_claimed', items: [{ n: 2 }, { n: 1 }], revision: 2, status: 'waiting' });
  assert.notDeepEqual(normalize({ status: 'waiting' }), normalize({ status: 'queued' }));
  assert.notDeepEqual(normalize({ items: [1, 2] }), normalize({ items: [2, 1] }));
});
