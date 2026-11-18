// Parses the hand-written data files in scripts/data/ (keys.md,
// diagnostics.md, webhook-events.md): `# Section` lines group entries,
// `## <name>` starts one, optional `key: value` lines follow it, then
// Markdown. The first fenced block of the entry's language is its example.
// Comments (<!-- -->) are dropped.
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

export const DATA_DIR = join(dirname(fileURLToPath(import.meta.url)), '..', 'data');

export function parseDataDoc(text, { exampleLang } = {}) {
  const sections = [];
  let section = null;
  let entry = null;
  let inFence = false;
  const lines = text.replace(/<!--[\s\S]*?-->/g, '').split('\n');
  const flush = () => {
    if (entry) {
      entry.body = entry.lines.join('\n').trim();
      delete entry.lines;
      if (exampleLang) {
        const fence = new RegExp('```' + exampleLang + '\\n([\\s\\S]*?)\\n```');
        const m = entry.body.match(fence);
        if (m) {
          entry.example = m[1];
          entry.body = entry.body.replace(m[0], '').replace(/\n{3,}/g, '\n\n').trim();
        }
      }
      section.entries.push(entry);
    }
    entry = null;
  };
  for (const line of lines) {
    if (line.startsWith('```')) inFence = !inFence;
    if (!inFence && /^# /.test(line)) {
      flush();
      if (section) section.intro = section.intro.join('\n').trim();
      section = { title: line.slice(2).trim(), intro: [], entries: [] };
      sections.push(section);
      continue;
    }
    if (!inFence && /^## /.test(line)) {
      flush();
      entry = { name: line.slice(3).trim(), meta: {}, lines: [], metaDone: false };
      continue;
    }
    if (entry) {
      const meta = !entry.metaDone && line.match(/^([a-z_]+): (.+)$/);
      if (meta) {
        entry.meta[meta[1]] = meta[2].trim();
        continue;
      }
      entry.metaDone = true;
      entry.lines.push(line);
    } else if (section) {
      section.intro.push(line);
    }
  }
  flush();
  if (section && Array.isArray(section.intro)) section.intro = section.intro.join('\n').trim();
  for (const s of sections) for (const e of s.entries) delete e.metaDone;
  return sections;
}

export function loadDataDoc(name, opts) {
  return parseDataDoc(readFileSync(join(DATA_DIR, name), 'utf8'), opts);
}

// ---------------------------------------------------- the key tree ----

// Every key the tree describes, in tree order: { path, shape }. Objects that
// are `same_as` another stop there; their keys are documented once.
export function treeKeys(shape, out = [], extra = new Set()) {
  if (!shape) return out;
  if (shape.kind === 'object' && shape.keys) {
    for (const k of shape.keys) {
      out.push({ path: k.path, shape: k.shape });
      treeKeys(k.shape, out, extra);
    }
  } else if (shape.kind === 'list') {
    treeKeys(shape.item, out, extra);
  } else if (shape.kind === 'map') {
    treeKeys(shape.entry, out, extra);
  }
  return out;
}

// Paths that may be documented but needn't be: map entries such as
// `spec.inputs.<name>`.
export function optionalPaths(shape, parent = '', out = new Set()) {
  if (!shape) return out;
  if (shape.kind === 'object' && shape.keys) {
    for (const k of shape.keys) optionalPaths(k.shape, k.path, out);
  } else if (shape.kind === 'list') {
    optionalPaths(shape.item, `${parent}[]`, out);
  } else if (shape.kind === 'map') {
    out.add(`${parent}.<name>`);
    optionalPaths(shape.entry, `${parent}.<name>`, out);
  }
  return out;
}

// A field declaration takes every input declaration key: those entries are
// documented once, under spec.inputs.<name>.
export const INHERITED = [['spec.fields.<name>.', 'spec.inputs.<name>.']];

export function inheritedFrom(path) {
  for (const [from, to] of INHERITED) {
    if (path.startsWith(from)) return to + path.slice(from.length);
  }
  return null;
}

// Wraps a key example into a complete runbook for `runbook spec validate`.
export function wrapKeyExample(entry) {
  const indent = (text, n) =>
    text
      .split('\n')
      .map((l) => (l ? ' '.repeat(n) + l : l))
      .join('\n');
  switch (entry.meta.as) {
    case 'doc':
      return entry.example + '\n';
    case 'step':
      return `metadata: { id: key-example }\nspec:\n  steps:\n${indent(entry.example, 4)}\n`;
    case 'inputs':
      return `metadata: { id: key-example }\nspec:\n  inputs:\n${indent(entry.example, 4)}\n  steps:\n    - id: example_step\n`;
    default:
      throw new Error(`${entry.name}: unknown example wrapping "${entry.meta.as}"`);
  }
}
