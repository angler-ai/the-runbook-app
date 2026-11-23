import assert from 'node:assert/strict';
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');

function walk(dir) {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? walk(path) : [path];
  });
}

function resolveImport(from, specifier) {
  const base = resolve(dirname(from), specifier);
  for (const candidate of [base, `${base}.ts`, `${base}.tsx`, `${base}.d.ts`, join(base, 'index.ts')]) {
    if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
  }
  assert.fail(`${relative(root, from)} imports ${specifier}, which does not resolve`);
}

// Every repository file outside web/ that the web UI reaches through
// relative imports, following imports inside those files too.
function externalSources() {
  const web = join(root, 'web');
  const pending = walk(join(web, 'src')).filter((path) => /\.(ts|tsx)$/.test(path));
  const seen = new Set();
  const external = new Set();
  while (pending.length) {
    const file = pending.pop();
    if (seen.has(file)) continue;
    seen.add(file);
    const text = readFileSync(file, 'utf8');
    for (const [, specifier] of text.matchAll(/(?:from|import)\s*\(?\s*'(\.{1,2}\/[^']+)'/g)) {
      const path = specifier.replace(/\?.*$/, '');
      if (/\.(css|svg|png|wasm|json|txt)$/.test(path)) continue;
      const target = resolveImport(file, path);
      if (!target.startsWith(`${web}/`)) external.add(relative(root, target));
      pending.push(target);
    }
  }
  return [...external].sort();
}

test('the core image web stage copies every source the web UI imports from outside web/', () => {
  const dockerfile = readFileSync(join(root, 'Dockerfile'), 'utf8');
  const web = dockerfile.split(/\bAS web\n/)[1].split(/^FROM /m)[0];
  const copies = Array.from(web.matchAll(/^COPY (?!--from)(.+) (\S+)$/gm)).flatMap(([, sources, target]) =>
    sources.split(/\s+/).map((source) => ({ source, target })));
  const copied = (path) => copies.some(({ source, target }) =>
    (path === source && (target === `/src/${path}` || target === `/src/${dirname(path)}/`)) ||
    (path.startsWith(`${source}/`) && target === `/src/${source}`));
  const sources = externalSources();
  assert.ok(sources.some((path) => path.startsWith('runner/packages/shared/src/')));
  for (const path of sources) assert.ok(copied(path), `web stage must copy ${path}`);
});
