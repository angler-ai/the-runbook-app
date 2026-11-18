// Runs under `node --test` (no Vitest, no DOM): the registry is plain data.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { collectGalleryEntries, defineGallery, type GalleryModule } from './registry.ts';

const entry = (id: string, group: string, title: string) =>
  defineGallery({ id, group, title, examples: [] });

test('flattens modules and sorts by group then title', () => {
  const modules: Record<string, GalleryModule> = {
    './b.gallery.tsx': { default: [entry('zeta', 'B', 'Zeta'), entry('alpha', 'B', 'Alpha')] },
    './a.gallery.tsx': { default: entry('one', 'A', 'One') },
  };
  assert.deepEqual(
    collectGalleryEntries(modules).map((e) => e.id),
    ['one', 'alpha', 'zeta'],
  );
});

test('rejects duplicate ids and naming both files', () => {
  assert.throws(
    () =>
      collectGalleryEntries({
        './a.gallery.tsx': { default: entry('dup', 'A', 'A') },
        './b.gallery.tsx': { default: entry('dup', 'B', 'B') },
      }),
    /registered by \.\/a\.gallery\.tsx and \.\/b\.gallery\.tsx/,
  );
});

test('rejects ids that are not URL-safe', () => {
  assert.throws(
    () => collectGalleryEntries({ './a.gallery.tsx': { default: entry('Bad Id', 'A', 'A') } }),
    /must match/,
  );
});
