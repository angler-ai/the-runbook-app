// The YAML view (M5): Monaco plus monaco-yaml with the checked-in runbook JSON
// Schema (schemas/runbook-v1.json, imported at build time; nothing is fetched
// and nothing comes from a CDN). This module is the lazily loaded editor
// chunk: only the editor route imports it, through React.lazy.
//
// Workers: Vite emits Monaco's editor worker and monaco-yaml's worker as
// same-origin files, which the CSP's `worker-src 'self' blob:` allows.
// Compiler diagnostics arrive as markers under two owners, so provisional
// (browser) and server markers never mix.
import { useEffect, useRef } from 'react';
import { minimalChange } from './textChange';
import * as monaco from 'monaco-editor/editor/editor.api.js';
import 'monaco-editor/features/register.all.js';
import 'monaco-editor/languages/definitions/yaml/register.js';
import EditorWorker from 'monaco-editor/editor/editor.worker.js?worker';
import { configureMonacoYaml } from 'monaco-yaml';
import YamlWorker from 'monaco-yaml/yaml.worker.js?worker';
import runbookSchema from '../../../../schemas/runbook-v1.json';

export const PROVISIONAL_OWNER = 'runbook-browser-provisional';
export const SERVER_OWNER = 'runbook-server';

/** One diagnostic placed in the text. `provisional` marks the browser compiler's (F99). */
export interface EditorMarker {
  line: number;
  column: number;
  message: string;
  code: string;
  provisional: boolean;
}

function workerFor(label: string): Worker {
  return label === 'yaml' ? new YamlWorker() : new EditorWorker();
}

/**
 * monaco-yaml 5.5 (through monaco-worker-manager) still calls the pre-0.55
 * `createWebWorker({ moduleId, label, createData })`; monaco-editor 0.57 takes
 * `{ worker }` and leaves sending createData to the caller. This is the adapter
 * monaco 0.57 uses for its own language workers (internal/common/workers.js):
 * the first message starts the worker's initializer, the second is createData.
 */
function createLegacyWebWorker(opts: {
  label?: string;
  createData?: unknown;
  host?: Record<string, (...args: unknown[]) => unknown>;
  keepIdleModels?: boolean;
}) {
  const worker = workerFor(opts.label ?? '');
  worker.postMessage('ignore');
  worker.postMessage(opts.createData);
  return monaco.editor.createWebWorker({
    worker,
    host: opts.host,
    keepIdleModels: opts.keepIdleModels,
  });
}

let configured = false;
function configureOnce() {
  if (configured) return;
  configured = true;
  (globalThis as { MonacoEnvironment?: monaco.Environment }).MonacoEnvironment = {
    getWorker: (_moduleId: string, label: string) => workerFor(label),
  };
  const monacoForYaml = {
    ...monaco,
    editor: { ...monaco.editor, createWebWorker: createLegacyWebWorker },
  };
  configureMonacoYaml(monacoForYaml as unknown as Parameters<typeof configureMonacoYaml>[0], {
    enableSchemaRequest: false,
    validate: true,
    hover: true,
    completion: true,
    schemas: [
      {
        // An identifier only: the schema is inline, so the worker never fetches it.
        uri: 'https://therunbook.app/schemas/v1.json',
        fileMatch: ['*.runbook.yaml'],
        schema: runbookSchema as never,
      },
    ],
  });
}

function prefersDark(): boolean {
  const theme = document.documentElement.dataset.theme;
  if (theme === 'dark') return true;
  if (theme === 'light') return false;
  return window.matchMedia?.('(prefers-color-scheme: dark)').matches ?? false;
}

function toMarkerData(markers: EditorMarker[]): monaco.editor.IMarkerData[] {
  return markers.map((m) => ({
    severity: monaco.MarkerSeverity.Error,
    message: m.provisional ? `${m.message} (provisional: browser check)` : m.message,
    source: m.provisional ? 'browser (provisional)' : 'server',
    code: m.code,
    startLineNumber: m.line,
    startColumn: m.column,
    endLineNumber: m.line,
    endColumn: m.column + 1,
  }));
}

let modelSeq = 0;

export interface YamlEditorProps {
  /**
   * The source. Edits flow out through `onChange`; `value` is written back into
   * the editor only when `resetKey` changes (a reload), never on re-renders, so
   * a render that lags behind fast typing cannot undo keystrokes.
   */
  value: string;
  resetKey: number;
  /**
   * UI-4: bumped when the builder (or its undo) changed the text. The change
   * is applied as one edit over the part that differs, so the YAML view keeps
   * its cursor, scroll and undo history.
   */
  syncKey?: number;
  /** UI-4: put the cursor here and scroll to it (a parse error's link). */
  reveal?: { line: number; column: number; key: number } | null;
  onChange(value: string): void;
  markers: EditorMarker[];
  readOnly: boolean;
  /** Ctrl/Cmd+S. */
  onSave?: () => void;
  ariaLabel: string;
  /** Element id of the help text describing keyboard use. */
  describedBy?: string;
}

export default function YamlEditor({
  value,
  resetKey,
  syncKey = 0,
  reveal = null,
  onChange,
  markers,
  readOnly,
  onSave,
  ariaLabel,
  describedBy,
}: YamlEditorProps) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const editorRef = useRef<monaco.editor.IStandaloneCodeEditor | null>(null);
  const handlers = useRef({ onChange, onSave });
  /** True while the builder's change is being applied (it is not the author typing). */
  const applying = useRef(false);
  useEffect(() => {
    handlers.current = { onChange, onSave };
  });

  useEffect(() => {
    configureOnce();
    const host = hostRef.current;
    if (!host) return undefined;
    // A fresh model per mount: a model left over from another runbook never leaks in.
    modelSeq += 1;
    const model = monaco.editor.createModel(
      value,
      'yaml',
      monaco.Uri.parse(`inmemory://runbook/draft-${modelSeq}.runbook.yaml`),
    );
    const editor = monaco.editor.create(host, {
      model,
      theme: prefersDark() ? 'vs-dark' : 'vs',
      automaticLayout: true,
      minimap: { enabled: false },
      fontSize: 13,
      tabSize: 2,
      insertSpaces: true,
      scrollBeyondLastLine: false,
      readOnly,
      ariaLabel,
      accessibilitySupport: 'auto',
    });
    editorRef.current = editor;
    const textarea = host.querySelector('textarea');
    if (textarea && describedBy) textarea.setAttribute('aria-describedby', describedBy);
    const changes = model.onDidChangeContent(() => {
      if (!applying.current) handlers.current.onChange(model.getValue());
    });
    editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS, () =>
      handlers.current.onSave?.(),
    );

    const media = window.matchMedia?.('(prefers-color-scheme: dark)');
    const applyTheme = () => monaco.editor.setTheme(prefersDark() ? 'vs-dark' : 'vs');
    media?.addEventListener('change', applyTheme);
    const observer = new MutationObserver(applyTheme);
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme'],
    });
    return () => {
      observer.disconnect();
      media?.removeEventListener('change', applyTheme);
      changes.dispose();
      editor.dispose();
      model.dispose();
      editorRef.current = null;
    };
    // The editor is created once per mount; value, markers and readOnly sync below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const model = editorRef.current?.getModel();
    if (model && model.getValue() !== value) model.setValue(value);
    // Only a new resetKey replaces the text; `value` alone must not (see the prop).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [resetKey]);

  useEffect(() => {
    const model = editorRef.current?.getModel();
    if (!model || syncKey === 0) return;
    const change = minimalChange(model.getValue(), value);
    if (!change) return;
    const from = model.getPositionAt(change.start);
    const to = model.getPositionAt(change.end);
    applying.current = true;
    try {
      model.pushEditOperations(
        [],
        [
          {
            range: new monaco.Range(from.lineNumber, from.column, to.lineNumber, to.column),
            text: change.text,
          },
        ],
        () => null,
      );
    } finally {
      applying.current = false;
    }
    // Only a new syncKey applies the builder's text (see the prop).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [syncKey]);

  useEffect(() => {
    const editor = editorRef.current;
    if (!editor || !reveal) return;
    editor.revealLineInCenter(reveal.line);
    editor.setPosition({ lineNumber: reveal.line, column: reveal.column });
    editor.focus();
  }, [reveal]);

  useEffect(() => {
    editorRef.current?.updateOptions({ readOnly });
  }, [readOnly]);

  useEffect(() => {
    const model = editorRef.current?.getModel();
    if (!model) return;
    const lines = model.getLineCount();
    const inText = markers.filter((m) => m.line >= 1 && m.line <= lines);
    monaco.editor.setModelMarkers(
      model,
      PROVISIONAL_OWNER,
      toMarkerData(inText.filter((m) => m.provisional)),
    );
    monaco.editor.setModelMarkers(
      model,
      SERVER_OWNER,
      toMarkerData(inText.filter((m) => !m.provisional)),
    );
  }, [markers, value]);

  return <div ref={hostRef} data-testid="yaml-editor" className="h-[min(70vh,640px)] min-h-80" />;
}
