import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

test('the core image web stage copies the runner shared sources imported by the web UI', () => {
  const dockerfile = readFileSync(new URL('../../Dockerfile', import.meta.url), 'utf8');
  const web = dockerfile.split(/\bAS web\n/)[1].split(/^FROM /m)[0];
  for (const [source, destination] of [
    ['credential-manifest.ts', ''],
    ['oauth-binding.ts', ''],
    ['contracts/core.ts', 'contracts/'],
  ]) {
    const copy = /^COPY (.+) (\S+)$/gm;
    assert.ok(Array.from(web.matchAll(copy)).some(([, sources, target]) =>
      sources.split(/\s+/).includes(`runner/packages/shared/src/${source}`) &&
      target === `/src/runner/packages/shared/src/${destination}`), `web stage must copy ${source}`);
  }
});
