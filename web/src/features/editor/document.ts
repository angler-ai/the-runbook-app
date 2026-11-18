// The one YAML Document model behind every editor surface (plan §1, M5).
// The source text is the truth: the YAML editor writes text, and the parsed
// `yaml` Document is a lazily built view over it. Nothing reserializes over
// the author's formatting except an explicit `commit()`, which the deferred
// builder will call after mutating `document` (outline edits, step forms).
import { isNode, LineCounter, parseDocument, type Document } from 'yaml';

export interface RunbookMetadata {
  id?: string;
  title?: string;
}

export interface SourcePosition {
  /** 1-based, like compiler diagnostics and Monaco. */
  line: number;
  column: number;
}

/** RFC 6901 pointer (`/spec/steps/0/id`) to YAML path segments. */
export function pointerSegments(pointer: string): (string | number)[] {
  if (pointer === '' || pointer === '/') return [];
  return pointer
    .replace(/^\//, '')
    .split('/')
    .map((raw) => raw.replace(/~1/g, '/').replace(/~0/g, '~'))
    .map((segment) => (/^(0|[1-9][0-9]*)$/.test(segment) ? Number(segment) : segment));
}

export class RunbookDocument {
  #text: string;
  #parsed: { document: Document; lines: LineCounter } | null = null;

  constructor(text: string) {
    this.#text = text;
  }

  get text(): string {
    return this.#text;
  }

  /** Replace the source (an edit, a reload or a save). The view reparses on demand. */
  setText(text: string): void {
    if (text === this.#text) return;
    this.#text = text;
    this.#parsed = null;
  }

  #view() {
    if (!this.#parsed) {
      const lines = new LineCounter();
      this.#parsed = { document: parseDocument(this.#text, { lineCounter: lines }), lines };
    }
    return this.#parsed;
  }

  /** The parsed view. A builder mutates it and then calls `commit()`. */
  get document(): Document {
    return this.#view().document;
  }

  /** Serialize a builder mutation back into the source of truth. */
  commit(): string {
    this.#text = String(this.#view().document);
    this.#parsed = null;
    return this.#text;
  }

  metadata(): RunbookMetadata {
    const document = this.#view().document;
    const id = document.getIn(['metadata', 'id']);
    const title = document.getIn(['metadata', 'title']);
    return {
      ...(typeof id === 'string' ? { id } : {}),
      ...(typeof title === 'string' ? { title } : {}),
    };
  }

  /**
   * Where a diagnostic's JSON pointer starts in the text, for diagnostics the
   * compiler reports without a span. Walks up to the nearest existing parent.
   */
  locate(pointer: string): SourcePosition | null {
    const { document, lines } = this.#view();
    const segments = pointerSegments(pointer);
    for (let depth = segments.length; depth >= 0; depth -= 1) {
      const node: unknown = document.getIn(segments.slice(0, depth), true);
      if (isNode(node) && node.range) {
        const { line, col } = lines.linePos(node.range[0]);
        return { line, column: col };
      }
    }
    return null;
  }

  /** YAML syntax errors only; semantic diagnostics come from the compiler. */
  get syntaxErrors(): readonly { message: string }[] {
    return this.#view().document.errors;
  }
}
