// Component gallery registry (A11). The dev-only /__gallery route renders every
// registered example in light and dark, side by side; Playwright screenshots it.
//
// To register components, add a `*.gallery.tsx` file next to them whose default
// export is `defineGallery({...})` (or an array of them). The gallery discovers
// these files with import.meta.glob, so no central list needs editing.
//
// Each example renders twice (light and dark), so it must not hard-code DOM ids:
// put stateful or id-bearing demos in a small component that calls useId().
import type { ReactNode } from 'react';

export type GalleryExample = {
  /** Short label shown above the example, e.g. "Destructive". */
  name: string;
  render: () => ReactNode;
};

export type GalleryEntry = {
  /** Stable, unique, URL-safe id (used by /__gallery?entry=<id>). */
  id: string;
  title: string;
  /** Sidebar group, e.g. "Primitives" or "Execution". */
  group: string;
  examples: GalleryExample[];
};

export type GalleryModule = { default: GalleryEntry | GalleryEntry[] };

export function defineGallery(entry: GalleryEntry): GalleryEntry {
  return entry;
}

const ID = /^[a-z0-9][a-z0-9-]*$/;

/** Flattens the discovered modules, checks ids, and sorts by group then title. */
export function collectGalleryEntries(modules: Record<string, GalleryModule>): GalleryEntry[] {
  const seen = new Map<string, string>();
  const entries: GalleryEntry[] = [];
  for (const [file, mod] of Object.entries(modules)) {
    const list = Array.isArray(mod.default) ? mod.default : [mod.default];
    for (const entry of list) {
      if (!ID.test(entry.id)) {
        throw new Error(`Gallery entry id "${entry.id}" in ${file} must match ${ID}.`);
      }
      const previous = seen.get(entry.id);
      if (previous !== undefined) {
        throw new Error(`Gallery entry id "${entry.id}" is registered by ${previous} and ${file}.`);
      }
      seen.set(entry.id, file);
      entries.push(entry);
    }
  }
  return entries.sort((a, b) => a.group.localeCompare(b.group) || a.title.localeCompare(b.title));
}
