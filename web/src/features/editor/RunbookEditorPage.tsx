// The runbook editor (M5; UI-4 the builder). Graph (the builder), YAML and
// Outline tabs over one draft whose text is the source of truth: the
// builder edits the YAML (minimal splices, see features/builder), the YAML
// tab edits it as text, and both read it back. Browser diagnostics are
// provisional (F99); the diagnostics a save or publish returns are the
// server's and win for the text they were computed from. A 412 never
// overwrites anything silently: the author reloads and reapplies their
// changes, reloads theirs, or explicitly keeps theirs and retries.
//
// UI-4: a new runbook opens here unsaved (`/p/$project/runbooks/new`); its
// first save creates it as a draft (runbooks:write) and the editor moves to
// its own URL. Publishing stays a separate action (runbooks:publish).
import { useQueryClient } from '@tanstack/react-query';
import { Link, useBlocker, useNavigate } from '@tanstack/react-router';
import { lazy, Suspense, useCallback, useEffect, useId, useMemo, useRef, useState } from 'react';
import { ApiError, apiRuntime, newActionId } from '../../api/client';
import { queryKeys } from '../../api/query-keys';
import { useMe, useMeta } from '../../api/hooks';
import { useOrgContext } from '../context/OrgContext';
import { useCompileContext, type DirectoryGap } from '../compile/context';
import { useCompiler, type CompilerBackend } from '../compile/useCompiler';
import { findRunbookBySlug, useRunbookCatalog } from '../runbooks/hooks';
import { slugify } from '../runbooks/catalog';
import type { Diagnostic } from '../../wasm/runbook';
import type { Outline } from '../../ui/types';
import { EmptyState, ErrorState, PageHeader, Skeleton } from '../../ui/layout';
import { RunbookOutline } from '../../ui/outline';
import { Button } from '../../ui/primitives/button';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '../../ui/primitives/tabs';
import { BuilderView } from '../builder/BuilderView';
import { webhookTriggerUrl } from '../runbook/triggers';
import type { BuilderDirectory, CallTarget, EditOptions } from '../builder/context';
import { useDraftHistory } from '../builder/history';
import { reapply } from '../builder/merge';
import { readBuilder, stepForPath, RUNBOOK_ID } from '../builder/model';
import {
  templateById,
  templateSource,
  type NewRunbookSearch,
  type TemplateId,
} from '../builder/templates';
import { RunbookDocument } from './document';
import {
  draftQueryOptions,
  editorPermissions,
  isDefinitive,
  patchDraft,
  problemDiagnostics,
  runbookQueryOptions,
  detachFromGit,
  useCreateRunbook,
  useDraft,
  usePublish,
  useRunbook,
  useSaveDraft,
  type LoadedDraft,
  type RunbookView,
} from './hooks';
import type { EditorMarker } from './YamlEditor';
import { diagnosticMessage } from './diagnostics';
import { createErrorText } from './CreateRunbookDialog';
import { lastEditorTab, rememberEditorTab } from '../runbook/tabs';

const YamlEditor = lazy(() => import('./YamlEditor'));

/**
 * UI-4b: the browser says it is offline. Saves are still sent (a server on
 * localhost or the LAN may well answer; review 1 of UI-4b), but one that
 * then fails without an answer while offline is taken as never sent, so it
 * is not remembered as possibly landed.
 */
function offline(): boolean {
  return typeof navigator !== 'undefined' && navigator.onLine === false;
}

const OFFLINE = {
  tone: 'alert',
  text: 'You appear to be offline: the save did not reach the server. Save again once you are back online.',
} as const;

export type { NewRunbookSearch };

export interface RunbookEditorPageProps {
  projectRef: string;
  /** Absent: a new runbook, not saved yet. */
  runbookId?: string;
  /** For a new runbook: the template, name and ID it starts from. */
  start?: NewRunbookSearch;
  /** Test seam for the compiler (defaults to the shared Web Worker). */
  compiler?: CompilerBackend;
}

/**
 * The server's answer to a runbook's creation, handed to the editor that
 * opens at the new runbook's own URL (the first save moves there).
 */
const createdNotes = new Map<string, { diagnostics: Diagnostic[]; revision: number }>();

export function RunbookEditorPage({
  projectRef,
  runbookId,
  start,
  compiler,
}: RunbookEditorPageProps) {
  if (!runbookId)
    return <NewRunbookEditor projectRef={projectRef} start={start ?? {}} compiler={compiler} />;
  return (
    <ExistingRunbookEditor projectRef={projectRef} runbookId={runbookId} compiler={compiler} />
  );
}

function Loading({ what }: { what: string }) {
  return (
    <>
      <PageHeader title="Editor" />
      <div aria-busy="true" className="grid gap-3">
        <p role="status" className="sr-only">
          Loading {what}…
        </p>
        <Skeleton className="h-96 w-full" />
      </div>
    </>
  );
}

function ExistingRunbookEditor({
  projectRef,
  runbookId,
  compiler,
}: {
  projectRef: string;
  runbookId: string;
  compiler: CompilerBackend | undefined;
}) {
  const me = useMe();
  const draft = useDraft(runbookId);
  const runbook = useRunbook(runbookId);

  if (draft.isError) {
    const error = draft.error;
    const forbidden = error instanceof ApiError && error.status === 403;
    return (
      <>
        <PageHeader title="Editor" />
        <ErrorState
          title={forbidden ? 'You cannot read this runbook' : 'The draft could not be loaded'}
          message={
            forbidden
              ? 'Your token or project role does not allow reading runbooks here.'
              : undefined
          }
          code={error instanceof ApiError ? error.code : undefined}
          requestId={error instanceof ApiError ? (error.requestId ?? undefined) : undefined}
          onRetry={forbidden ? undefined : () => void draft.refetch()}
        />
      </>
    );
  }
  if (!draft.data || !me.data) return <Loading what="the draft" />;
  return (
    <EditorSession
      key={runbookId}
      projectRef={projectRef}
      runbookId={runbookId}
      orgId={me.data.org.id}
      scopes={me.data.credential.scopes}
      initial={draft.data}
      runbook={runbook.data}
      runbookPending={runbook.isPending}
      compiler={compiler}
    />
  );
}

function NewRunbookEditor({
  projectRef,
  start,
  compiler,
}: {
  projectRef: string;
  start: NewRunbookSearch;
  compiler: CompilerBackend | undefined;
}) {
  const me = useMe();
  if (!me.data) return <Loading what="the editor" />;
  return <NewSession projectRef={projectRef} start={start} me={me.data} compiler={compiler} />;
}

/** The template's text for the dialog's choice, fixed for the page's life. */
export function newRunbookSource(start: NewRunbookSearch, me?: string): string {
  const name = start.name?.trim() || 'New runbook';
  const id = start.id && RUNBOOK_ID.test(start.id) ? start.id : slugify(name) || 'new-runbook';
  return templateSource(templateById(start.template as TemplateId | undefined), {
    id,
    title: name,
    ...(me ? { me } : {}),
  });
}

function NewSession({
  projectRef,
  start,
  me,
  compiler,
}: {
  projectRef: string;
  start: NewRunbookSearch;
  me: NonNullable<ReturnType<typeof useMe>['data']>;
  compiler: CompilerBackend | undefined;
}) {
  const [initial] = useState<LoadedDraft>(() => ({
    draft: { runbook_id: '', source: newRunbookSource(start, me.user?.name), revision: 0 },
    etag: null,
  }));
  return (
    <EditorSession
      projectRef={projectRef}
      orgId={me.org.id}
      scopes={me.credential.scopes}
      initial={initial}
      runbook={undefined}
      runbookPending={false}
      compiler={compiler}
    />
  );
}

/** The revision the author's text is based on: what If-Match names. */
interface Base {
  source: string;
  etag: string | null;
  revision: number;
}

/** Diagnostics from the server, valid only for exactly this text. */
interface ServerDiagnostics {
  source: string;
  revision: number;
  diagnostics: Diagnostic[];
}

type Notice = { tone: 'status' | 'alert'; text: string };

/** A create that got no answer (D20): its key may carry only `source` and `slug`. */
interface PendingCreate {
  id: string;
  source: string;
  slug: string;
  /** UI-4b: the runbook it made, once the server said so (a replay of it). */
  runbookId?: string;
  /**
   * UI-4b: texts this person's own unanswered requests may have left as
   * that runbook's draft: the create's, then every save over it that got
   * no answer.
   */
  sent: string[];
}
type Conflict = { action: 'save' | 'publish' };

interface EditorSessionProps {
  projectRef: string;
  /** Absent for a new runbook (not saved yet). */
  runbookId?: string;
  orgId: string;
  scopes: readonly string[];
  initial: LoadedDraft;
  runbook: RunbookView | undefined;
  /** The runbook's catalog row (origin: Git or not) is still loading. */
  runbookPending: boolean;
  compiler: CompilerBackend | undefined;
}

function EditorSession({
  projectRef,
  runbookId,
  orgId,
  scopes,
  initial,
  runbook,
  runbookPending,
  compiler,
}: EditorSessionProps) {
  const isNew = runbookId === undefined;
  const queryClient = useQueryClient();
  const { resolveProject } = useOrgContext();
  const project = resolveProject(projectRef);
  const context = useCompileContext(orgId, project?.id);
  // The draft hooks need an ID; a new runbook has none until it is created.
  const saveDraft = useSaveDraft(runbookId ?? '');
  const publishDraft = usePublish(runbookId ?? '');
  const createRunbook = useCreateRunbook(project?.id ?? '');
  const catalog = useRunbookCatalog(project?.id);
  const meta = useMeta();

  const [source, setSource] = useState(initial.draft.source);
  // A new runbook's base is its template: untouched, it has nothing unsaved
  // to lose (review round 1); saving it is still allowed (see `canSave`).
  const [base, setBase] = useState<Base>(
    isNew
      ? { source: initial.draft.source, etag: null, revision: 0 }
      : { source: initial.draft.source, etag: initial.etag, revision: initial.draft.revision },
  );
  const [server, setServer] = useState<ServerDiagnostics | null>(() => {
    const note = runbookId ? createdNotes.get(runbookId) : undefined;
    return note
      ? { source: initial.draft.source, revision: note.revision, diagnostics: note.diagnostics }
      : null;
  });
  const [conflict, setConflict] = useState<Conflict | null>(null);
  const [notice, setNotice] = useState<Notice | null>(() => {
    const note = runbookId ? createdNotes.get(runbookId) : undefined;
    if (!note || !runbookId) return null;
    createdNotes.delete(runbookId);
    const count = note.diagnostics.length;
    return {
      tone: 'status',
      text:
        count === 0
          ? `Created as a draft (revision ${note.revision}). The server found no problems. Publish when it is ready.`
          : `Created as a draft (revision ${note.revision}). The server found ${count} ${count === 1 ? 'problem' : 'problems'}; fix ${count === 1 ? 'it' : 'them'} before publishing.`,
    };
  });
  const [busy, setBusy] = useState<'save' | 'publish' | 'reload' | null>(null);
  // UI-2: Graph first, unless the person last chose another tab here.
  const [tab, setTab] = useState<string>(lastEditorTab);
  /** Bumped when the text is replaced from outside the editor (a reload). */
  const [resetKey, setResetKey] = useState(0);
  /** UI-4: bumped when the builder (or undo) changed the text. */
  const [syncKey, setSyncKey] = useState(0);
  const [reveal, setReveal] = useState<{ line: number; column: number; key: number } | null>(null);
  const [selected, setSelected] = useState<string | undefined>(undefined);
  const history = useDraftHistory();
  // A call step links to the child runbook's page; `$rb` takes a slug.
  const callHref = useCallback(
    (slug: string) => `/p/${encodeURIComponent(projectRef)}/runbooks/${encodeURIComponent(slug)}`,
    [projectRef],
  );
  // A saved runbook's webhook trigger URLs (a new one has no address yet).
  const triggerSlug = runbook?.slug ?? runbookId;
  const triggerUrl = useMemo(
    () =>
      triggerSlug
        ? (id: string) =>
            webhookTriggerUrl({
              origin: window.location.origin,
              project: projectRef,
              runbook: triggerSlug,
              trigger: id,
            })
        : undefined,
    [projectRef, triggerSlug],
  );
  /** The publish action in flight or awaiting retry: one Idempotency-Key per action (D20). */
  const publishAction = useRef<{ id: string; latestBefore: number | null } | null>(null);
  /**
   * The creation in flight or awaiting retry (D20), for a new runbook, with
   * the text and slug it sent: its Idempotency-Key may carry only that body.
   */
  const createAction = useRef<PendingCreate | null>(null);
  /** Set while leaving on purpose (after creating), so the unsaved-changes guard stays quiet. */
  const leaving = useRef(false);
  /**
   * Review round 1: texts sent in saves that got no answer (a network error,
   * a 5xx). Each may have become a revision, so "Reload and reapply" must
   * not treat its changes as someone else's (they would be applied twice).
   */
  const unconfirmed = useRef<string[]>([]);

  // The shared Document model over the current text (parsed lazily, only when a
  // diagnostic without a span has to be placed).
  const documentModel = useMemo(() => new RunbookDocument(source), [source]);
  const builderRead = useMemo(() => readBuilder(source), [source]);

  const { canWrite: scopeWrite, canPublish: scopePublish } = editorPermissions(scopes);
  const gitManaged = runbook?.origin === 'git';
  // Review round 1: whether it is Git-managed is not known until the
  // runbook's row loads; until then nothing may be edited.
  const checkingOrigin = !isNew && runbookPending;
  // The server is the authority: a 403 (project role) or git_managed answer
  // turns the matching action off even when the token's scopes allow it.
  const [refused, setRefused] = useState({ write: false, publish: false });
  const readOnly = !scopeWrite || gitManaged || refused.write || checkingOrigin;
  const canPublish = !isNew && scopePublish && !gitManaged && !refused.publish && !checkingOrigin;

  const compiled = useCompiler(source, {
    context: context.data?.directory,
    enabled: !context.isLoading,
    backend: compiler,
  });

  const dirty = source !== base.source;
  /** A new runbook can be saved untouched (its template); a draft only with changes. */
  const canSave = isNew || dirty;
  const serverCurrent = server !== null && server.source === source ? server : null;
  const provisional = compiled.validate?.diagnostics ?? null;
  const shown = useMemo<Diagnostic[]>(
    () => serverCurrent?.diagnostics ?? provisional ?? [],
    [serverCurrent, provisional],
  );
  const fromServer = serverCurrent !== null;

  const locate = useCallback(
    (d: Diagnostic) =>
      d.line != null && d.line > 0
        ? { line: d.line, column: d.column != null && d.column > 0 ? d.column : 1 }
        : (documentModel.locate(d.path) ?? { line: 1, column: 1 }),
    [documentModel],
  );

  const markers = useMemo<EditorMarker[]>(
    () =>
      shown.map((d) => ({
        ...locate(d),
        message: diagnosticMessage(d),
        code: d.code,
        provisional: !fromServer,
      })),
    [shown, fromServer, locate],
  );

  // The last outline that compiled stays on screen while the text does not parse.
  const outline = builderRead.ok ? builderRead.model.outline : null;
  const [lastOutline, setLastOutline] = useState<Outline | null>(null);
  if (outline && outline !== lastOutline) setLastOutline(outline);
  const shownOutline = outline ?? lastOutline;

  const directory = useMemo<BuilderDirectory>(
    () => ({
      users: (context.data?.directory.users ?? []).map((u) => u.name),
      teams: (context.data?.directory.teams ?? []).map((t) => t.slug),
      skills: (context.data?.directory.skills ?? []).map((s) => s.name),
    }),
    [context.data],
  );
  const callTargets = useMemo<CallTarget[]>(
    () =>
      (catalog.data?.pages ?? [])
        .flatMap((page) => page.items)
        .filter((row) => row.latest_version != null && row.runbook_id !== runbookId)
        .map((row) => ({
          runbookId: row.runbook_id,
          slug: row.slug,
          title: row.title ?? row.slug,
          latestVersion: row.latest_version as number,
        })),
    [catalog.data, runbookId],
  );

  useBeforeUnload(dirty);
  // In-app navigation (the graph's call links go through the router, with
  // no reload) asks the same question the browser asks on unload.
  useBlocker({
    shouldBlockFn: () =>
      !leaving.current && !window.confirm('You have unsaved changes. Leave the editor anyway?'),
    disabled: !dirty,
    enableBeforeUnload: false,
  });
  const navigate = useNavigate();
  const follow = useCallback((href: string) => void navigate({ href }), [navigate]);

  // Recorded here, not in a state updater (StrictMode runs those twice).
  /** The YAML tab typed: one undo step per burst of typing. */
  const onYamlChange = useCallback(
    (next: string) => {
      history.record(source, next, 'yaml');
      setSource(next);
    },
    [history, source],
  );

  /** The builder changed the text. */
  const onBuilderEdit = useCallback(
    (next: string, options: EditOptions) => {
      history.record(source, next, options.coalesce);
      setSource(next);
      setSyncKey((key) => key + 1);
    },
    [history, source],
  );

  const builderHistory = useMemo(
    () => ({
      canUndo: history.canUndo,
      canRedo: history.canRedo,
      undo: () => {
        const previous = history.undo(source);
        if (previous === null) return;
        setSource(previous);
        setSyncKey((key) => key + 1);
      },
      redo: () => {
        const next = history.redo(source);
        if (next === null) return;
        setSource(next);
        setSyncKey((key) => key + 1);
      },
    }),
    [history, source],
  );

  function showInYaml(position: { line: number; column: number }) {
    setTab('yaml');
    setReveal({ ...position, key: Date.now() });
  }

  function showProblem(d: Diagnostic) {
    const step = builderRead.ok ? stepForPath(builderRead.model, d.path) : undefined;
    setTab('graph');
    setSelected(step?.graphId);
  }

  function failure(action: 'Save' | 'Publish', error: unknown): Notice {
    if (!(error instanceof ApiError)) {
      return { tone: 'alert', text: `${action} did not reach the server. Try again.` };
    }
    if (error.code === 'runbook.git_managed') {
      return {
        tone: 'alert',
        text: 'This runbook is managed in Git. Change it through a pull request.',
      };
    }
    if (error.status === 403) {
      return {
        tone: 'alert',
        text:
          action === 'Save'
            ? 'You may not edit this runbook: your token or project role lacks write access.'
            : 'You may not publish this runbook: your token or project role lacks runbooks:publish.',
      };
    }
    if (error.status === 413) {
      return { tone: 'alert', text: 'The source is larger than the 1 MiB limit.' };
    }
    if (action === 'Publish' && !isDefinitive(error)) {
      return {
        tone: 'alert',
        text: `Publishing did not finish (${error.code}). Publish again: the retry repeats the same request, so it cannot publish twice.`,
      };
    }
    return { tone: 'alert', text: `${action} failed (${error.code}).` };
  }

  async function freshDraft(): Promise<LoadedDraft> {
    return queryClient.fetchQuery({ ...draftQueryOptions(runbookId ?? ''), staleTime: 0 });
  }

  /** PATCH `text` over the revision `etag` names. Returns the saved base, or null after a failure. */
  async function writeDraft(
    text: string,
    etag: string | null,
  ): Promise<{ saved: Base; problems: number } | null> {
    if (!etag) {
      setNotice({ tone: 'alert', text: 'The draft has no revision tag. Reload it before saving.' });
      return null;
    }
    try {
      const { write, etag: next } = await saveDraft.mutateAsync({ source: text, etag });
      unconfirmed.current = [];
      const saved = { source: write.source, etag: next, revision: write.revision };
      setBase(saved);
      setServer({ source: write.source, revision: write.revision, diagnostics: write.diagnostics });
      setConflict(null);
      return { saved, problems: write.diagnostics.length };
    } catch (error) {
      // UI-4b: a save that failed with no answer while the browser is
      // offline never left it, so it is not remembered; one that was sent
      // and got no answer may have landed.
      const neverSent = !isDefinitive(error) && offline();
      if (!isDefinitive(error) && !neverSent) unconfirmed.current = [...unconfirmed.current, text];
      if (neverSent) {
        setNotice(OFFLINE);
        return null;
      }
      if (error instanceof ApiError && error.status === 412) {
        setConflict({ action: 'save' });
      } else {
        if (error instanceof ApiError && error.code === 'runbook.git_managed') {
          setRefused({ write: true, publish: true });
        } else if (error instanceof ApiError && error.status === 403) {
          setRefused((r) => ({ ...r, write: true }));
        }
        setNotice(failure('Save', error));
      }
      return null;
    }
  }

  function savedNotice(saved: Base, count: number): Notice {
    return {
      tone: 'status',
      text:
        count === 0
          ? `Saved revision ${saved.revision}. The server found no problems.`
          : `Saved revision ${saved.revision}. The server found ${count} ${count === 1 ? 'problem' : 'problems'}; they replace the browser's provisional ones.`,
    };
  }

  /** The first save of a new runbook creates it (a draft; nothing is published). */
  async function create() {
    if (!project) return;
    const id = builderRead.ok ? builderRead.model.outline.id : '';
    const slug = RUNBOOK_ID.test(id) ? id : '';
    const pending = createAction.current;
    if (pending && (pending.source !== source || pending.runbookId)) {
      // Review round 1: the last attempt got no answer, and the text has
      // changed since. Its key may not carry a new body (the server answers
      // request.idempotency_key_reused): if that attempt created the
      // runbook, save this text over it; if not, this is a new action.
      if (await saveOverLandedCreate(project.id, pending)) return;
      apiRuntime.forgetAction(pending.id);
      createAction.current = null;
    }
    const action = (createAction.current ??= {
      id: newActionId(),
      source,
      slug,
      sent: [source],
    });
    try {
      const { write } = await createRunbook.mutateAsync({
        source,
        slug,
        actionId: action.id,
      });
      createAction.current = null;
      await openCreated(write.runbook_id, write.diagnostics, write.revision);
    } catch (error) {
      // A definitive refusal ends this attempt; edits after it are a new action.
      if (isDefinitive(error)) createAction.current = null;
      setNotice({ tone: 'alert', text: createErrorText(error) });
    }
  }

  /** Move to the created runbook's own editor, carrying the server's answer. */
  async function openCreated(rb: string, diagnostics: Diagnostic[], revision: number) {
    createdNotes.set(rb, { diagnostics, revision });
    leaving.current = true;
    await navigate({
      to: '/p/$project/runbooks/$rb/editor',
      params: { project: projectRef, rb },
      replace: true,
    });
  }

  /**
   * If the unanswered attempt `pending` created its runbook, save the
   * current text over that draft and open it. False when it did not land.
   *
   * UI-4b: whether it landed, and which runbook it made, is the server's
   * answer to the same request replayed (its key and body; D20 binds a key
   * to the principal that sent it), so a runbook someone else created with
   * the same slug and text is never taken for this person's. A direct
   * lookup of the slug first spares the replay when nothing has it (the
   * replay would create the runbook from the old text). The draft found is
   * saved over only while it is one this person's own requests produced
   * (the create's text or a later save's that got no answer) or already
   * the current text.
   */
  async function saveOverLandedCreate(projectId: string, pending: PendingCreate): Promise<boolean> {
    try {
      let rb = pending.runbookId;
      if (!rb) {
        if (pending.slug && !(await findRunbookBySlug(projectId, pending.slug))) return false;
        try {
          const { write } = await createRunbook.mutateAsync({
            source: pending.source,
            slug: pending.slug,
            actionId: pending.id,
          });
          rb = write.runbook_id;
          pending.runbookId = rb;
        } catch (error) {
          // Refused (the slug is someone else's): that attempt made nothing.
          if (isDefinitive(error)) return false;
          throw error;
        }
      }
      const landed = await queryClient.fetchQuery({ ...draftQueryOptions(rb), staleTime: 0 });
      const ours = landed.draft.source === source || pending.sent.includes(landed.draft.source);
      if (!ours || !landed.etag) {
        setNotice({
          tone: 'alert',
          text: `Your runbook ${pending.slug || rb} was created, but its draft has changed since (revision ${landed.draft.revision}), so nothing was saved over it. Copy your text, then open it from the runbook list.`,
        });
        return true;
      }
      try {
        const { write, etag } = await patchDraft(rb, source, landed.etag);
        queryClient.setQueryData<LoadedDraft>(queryKeys.runbookDraft(rb), {
          draft: { runbook_id: write.runbook_id, source: write.source, revision: write.revision },
          etag,
        });
        createAction.current = null;
        await openCreated(rb, write.diagnostics, write.revision);
      } catch (error) {
        // Sent, no answer: it may now be the draft, and is still ours
        // (not when the browser is offline: then it never left).
        if (!isDefinitive(error) && !offline()) pending.sent.push(source);
        setNotice(!isDefinitive(error) && offline() ? OFFLINE : failure('Save', error));
      }
      return true;
    } catch (error) {
      setNotice(failure('Save', error));
      return true;
    }
  }

  async function save() {
    if (readOnly || busy || !canSave) return;
    setBusy('save');
    setNotice(null);
    if (isNew) {
      await create();
      setBusy(null);
      return;
    }
    const result = await writeDraft(source, base.etag);
    setBusy(null);
    if (result) setNotice(savedNotice(result.saved, result.problems));
  }

  async function publishFrom(current: Base) {
    if (!runbookId) return;
    let action = publishAction.current;
    if (!action) {
      // The latest version before this action began, to tell "unchanged" (D21) from new.
      const known = await queryClient
        .fetchQuery({ ...runbookQueryOptions(runbookId), staleTime: 0 })
        .catch(() => runbook);
      action = { id: newActionId(), latestBefore: known?.latest_version ?? null };
      publishAction.current = action;
    }
    try {
      const version = await publishDraft.mutateAsync({
        etag: current.etag ?? '',
        actionId: action.id,
      });
      publishAction.current = null;
      setServer({ source: current.source, revision: current.revision, diagnostics: [] });
      setConflict(null);
      const publication =
        version.runbook_version === action.latestBefore
          ? `Unchanged; same as v${version.runbook_version}. Nothing new was published.`
          : `Published v${version.runbook_version}.`;
      setNotice({
        tone: 'status',
        text: [publication, ...(version.warnings ?? []).map(({ message }) => message)].join(' '),
      });
    } catch (error) {
      if (isDefinitive(error)) publishAction.current = null;
      if (error instanceof ApiError && error.status === 412) {
        setConflict({ action: 'publish' });
      } else if (error instanceof ApiError && error.status === 422 && error.errors.length > 0) {
        const diagnostics = problemDiagnostics(error);
        setServer({ source: current.source, revision: current.revision, diagnostics });
        setNotice({
          tone: 'alert',
          text: `Not published: the server found ${diagnostics.length} ${diagnostics.length === 1 ? 'problem' : 'problems'}. They replace the browser's provisional ones.`,
        });
      } else {
        if (error instanceof ApiError && error.code === 'runbook.git_managed') {
          setRefused({ write: true, publish: true });
        } else if (error instanceof ApiError && error.status === 403) {
          setRefused((r) => ({ ...r, publish: true }));
        }
        setNotice(failure('Publish', error));
      }
    }
  }

  async function publish() {
    if (!canPublish || busy || dirty) return;
    setBusy('publish');
    setNotice(null);
    await publishFrom(base);
    setBusy(null);
  }

  /** Replace the text with the server's newest draft and start the history over. */
  function adopt(fresh: LoadedDraft, text: string) {
    setSource(text);
    setResetKey((key) => key + 1);
    setBase({ source: fresh.draft.source, etag: fresh.etag, revision: fresh.draft.revision });
    setServer(null);
    setConflict(null);
    history.reset();
    publishAction.current = null;
    unconfirmed.current = [];
  }

  async function reload() {
    setBusy('reload');
    try {
      const fresh = await freshDraft();
      adopt(fresh, fresh.draft.source);
      setNotice({
        tone: 'status',
        text: `Loaded revision ${fresh.draft.revision} from the server. Your unsaved edits were discarded.`,
      });
    } catch (error) {
      setNotice({
        tone: 'alert',
        text: `The draft could not be reloaded${error instanceof ApiError ? ` (${error.code})` : ''}.`,
      });
    } finally {
      setBusy(null);
    }
  }

  /** UI-4: load the newer draft and apply this author's changes on top of it. */
  async function reloadAndReapply() {
    setBusy('reload');
    try {
      const fresh = await freshDraft();
      const merged = reapply({
        base: base.source,
        unconfirmed: unconfirmed.current,
        mine: source,
        theirs: fresh.draft.source,
      });
      if (!merged.ok) {
        setNotice({
          tone: 'alert',
          text:
            merged.reason === 'unconfirmed'
              ? `An earlier save of yours was sent but no answer came back, so the server may have saved it under revision ${fresh.draft.revision}, and your changes could not be reapplied safely. Reload theirs (yours are discarded) or keep yours.`
              : `Your changes overlap revision ${fresh.draft.revision}'s, so they could not be reapplied on their own. Reload theirs (yours are discarded) or keep yours.`,
        });
        return;
      }
      adopt(fresh, merged.text);
      setNotice({
        tone: 'status',
        text: `Loaded revision ${fresh.draft.revision} and reapplied your changes on top. Review them, then save.`,
      });
    } catch (error) {
      setNotice({
        tone: 'alert',
        text: `The draft could not be reloaded${error instanceof ApiError ? ` (${error.code})` : ''}.`,
      });
    } finally {
      setBusy(null);
    }
  }

  /** Overwrite the newer draft with this text, by explicit choice, then finish the action. */
  async function keepMineAndRetry() {
    if (!conflict) return;
    const action = conflict.action;
    setBusy(action);
    setNotice(null);
    try {
      const fresh = await freshDraft();
      const theirs: Base = {
        source: fresh.draft.source,
        etag: fresh.etag,
        revision: fresh.draft.revision,
      };
      let current: Base | null = theirs;
      let problems = 0;
      if (source !== theirs.source) {
        const result = await writeDraft(source, theirs.etag);
        current = result?.saved ?? null;
        problems = result?.problems ?? 0;
      } else {
        // They saved exactly this text: adopt their revision, nothing to
        // overwrite. UI-4b: a save of ours that got no answer can no longer
        // land under it (its If-Match names an older revision), so it is
        // forgotten, as after any adopted or confirmed revision.
        setBase(theirs);
        setConflict(null);
        unconfirmed.current = [];
      }
      if (current && action === 'publish') {
        await publishFrom(current);
      } else if (current) {
        setNotice(savedNotice(current, problems));
      }
    } catch (error) {
      setNotice({
        tone: 'alert',
        text: `The newer draft could not be read${error instanceof ApiError ? ` (${error.code})` : ''}.`,
      });
    } finally {
      setBusy(null);
    }
  }

  const helpId = useId();
  const publishHintId = useId();
  const docTitle = builderRead.ok ? builderRead.model.outline.title : undefined;
  const docId = builderRead.ok ? builderRead.model.outline.id : undefined;
  const title = isNew
    ? (docTitle ?? 'New runbook')
    : (runbook?.title ?? runbook?.slug ?? runbookId);
  const stepBudget = compiled.validate?.step_budget;
  // The server's per-run step cap (`RUNBOOK_LIMIT_STEP_RUNS`), when it has one.
  const stepCap = meta.data?.limits?.step_runs ?? null;

  // Nothing to build without the right to save: say who can, instead of an
  // empty builder under a read-only notice.
  if (isNew && readOnly) {
    return (
      <EmptyState
        headingLevel={1}
        title="You can't create runbooks in this project"
        description="Creating a runbook needs an editor or admin role in the project (or, signed in with a token, the runbooks:write scope). Ask a project admin to change your role."
        action={
          <Link
            to="/p/$project/runbooks"
            params={{ project: projectRef }}
            className="text-sm font-medium text-accent hover:underline"
          >
            Back to Runbooks
          </Link>
        }
      />
    );
  }

  return (
    <>
      <PageHeader
        title={isNew ? 'New runbook' : 'Editor'}
        description={
          <span>
            {title}
            {isNew ? (
              docId ? (
                <span className="font-mono text-fg-muted"> · {docId}</span>
              ) : null
            ) : runbook ? (
              <span className="font-mono text-fg-muted">
                {' '}
                · {runbook.slug}
                {runbook.latest_version != null ? `@${runbook.latest_version}` : ''}
              </span>
            ) : null}
          </span>
        }
        actions={
          readOnly && !canPublish ? undefined : (
            <>
              {readOnly ? null : (
                <Button
                  variant={isNew ? 'primary' : 'secondary'}
                  disabled={!canSave || busy !== null}
                  onClick={() => void save()}
                >
                  {busy === 'save' ? 'Saving…' : 'Save draft'}
                </Button>
              )}
              {canPublish ? (
                <Button
                  disabled={dirty || busy !== null || !base.etag}
                  aria-describedby={dirty ? publishHintId : undefined}
                  onClick={() => void publish()}
                >
                  {busy === 'publish' ? 'Publishing…' : 'Publish'}
                </Button>
              ) : null}
            </>
          )
        }
      />

      {readOnly ? (
        <p className="mb-4 rounded-md border border-border p-3 text-sm" data-testid="read-only">
          {gitManaged ? (
            <>
              Read only: this runbook is managed in Git. Change it through a pull request to its
              repository; the builder shows it as it is.
              {scopePublish && runbookId ? (
                <>
                  {' '}
                  <Button
                    type="button"
                    variant="secondary"
                    size="sm"
                    onClick={() => {
                      if (
                        !globalThis.confirm(
                          'Edit this runbook here instead of in Git? The next sync of its file takes it back.',
                        )
                      )
                        return;
                      void detachFromGit(runbookId).then(() =>
                        queryClient.invalidateQueries({ queryKey: queryKeys.runbook(runbookId) }),
                      );
                    }}
                  >
                    Edit here instead
                  </Button>
                </>
              ) : null}
            </>
          ) : checkingOrigin && scopeWrite && !refused.write ? (
            'Read only while checking whether this runbook is managed in Git…'
          ) : (
            'Read only: your token or project role does not allow editing runbooks (runbooks:write).'
          )}
        </p>
      ) : null}

      <div className="mb-3 flex flex-wrap items-center gap-x-4 gap-y-1 text-sm">
        <p role="status" aria-live="polite">
          {isNew
            ? 'Not saved yet. Saving creates the runbook as a draft; publishing is separate.'
            : dirty
              ? 'Unsaved changes.'
              : `Saved, revision ${base.revision}.`}
          {dirty && canPublish ? <span id={publishHintId}> Save before publishing.</span> : null}
        </p>
        <p data-testid="step-budget">
          {stepBudget != null
            ? `Step budget: at most ${stepBudget} step runs (provisional).`
            : compiled.validate
              ? 'Step budget: unknown until the runbook compiles.'
              : 'Step budget: checking…'}
        </p>
        {stepBudget != null && stepCap != null && stepBudget > stepCap ? (
          <p role="note" data-testid="step-cap-warning" className="text-warning-text">
            Over this server’s limit of {stepCap} steps per run: a run that reaches it stops.
          </p>
        ) : null}
      </div>

      {conflict ? (
        <div
          role="alert"
          className="mb-4 grid gap-2 rounded-md border border-stale-border bg-stale-bg p-3 text-stale-fg"
          data-testid="conflict"
        >
          <p className="font-medium">This draft changed elsewhere.</p>
          <p className="text-sm">
            Someone saved a newer revision after you loaded it, so your{' '}
            {conflict.action === 'save' ? 'save' : 'publish'} was refused and nothing was
            overwritten. Reload it and reapply your changes on top, reload theirs (your unsaved
            edits are discarded), or keep yours: it replaces theirs as a new revision
            {conflict.action === 'publish' ? ', and then publishes' : ''}.
          </p>
          <div className="flex flex-wrap gap-2">
            <Button disabled={busy !== null} onClick={() => void reloadAndReapply()}>
              Reload and reapply my changes
            </Button>
            <Button variant="secondary" disabled={busy !== null} onClick={() => void reload()}>
              Reload their version
            </Button>
            <Button
              variant="secondary"
              disabled={busy !== null}
              onClick={() => void keepMineAndRetry()}
            >
              Keep mine and retry
            </Button>
          </div>
        </div>
      ) : null}

      {notice ? (
        <p
          role={notice.tone}
          className={
            notice.tone === 'alert'
              ? 'mb-4 rounded-md border border-verdict-fail-border bg-verdict-fail-bg p-3 text-verdict-fail-fg'
              : 'mb-4'
          }
          data-testid="editor-notice"
        >
          {notice.text}
        </p>
      ) : null}

      <DiagnosticsPanel
        diagnostics={shown}
        fromServer={fromServer}
        serverRevision={serverCurrent?.revision ?? null}
        compilerError={compiled.error}
        checking={!compiled.validate && !compiled.error}
        gaps={context.data?.gaps ?? []}
        contextFailed={context.isError}
        stepOf={(d) => (builderRead.ok ? stepForPath(builderRead.model, d.path)?.title : undefined)}
        onShowStep={showProblem}
        onShowLine={(d) => showInYaml(locate(d))}
      />

      <Tabs
        value={tab}
        onValueChange={(next) => {
          setTab(next);
          rememberEditorTab(next);
        }}
      >
        <TabsList aria-label="Editor views">
          <TabsTrigger value="graph">Graph</TabsTrigger>
          <TabsTrigger value="yaml">YAML</TabsTrigger>
          <TabsTrigger value="outline">Outline</TabsTrigger>
        </TabsList>
        <TabsContent value="yaml" forceMount className="data-[state=inactive]:hidden">
          <Suspense
            fallback={
              <div aria-busy="true">
                <p role="status" className="sr-only">
                  Loading the YAML editor…
                </p>
                <Skeleton className="h-96 w-full" />
              </div>
            }
          >
            <YamlEditor
              value={source}
              resetKey={resetKey}
              syncKey={syncKey}
              reveal={reveal}
              onChange={onYamlChange}
              markers={markers}
              readOnly={readOnly}
              onSave={() => void save()}
              ariaLabel={readOnly ? 'Runbook YAML (read only)' : 'Runbook YAML'}
              describedBy={helpId}
            />
          </Suspense>
          <p id={helpId} className="mt-2 text-sm text-fg-muted">
            Tab inserts indentation. To move focus out of the editor with Tab, press Ctrl+M
            (Ctrl+Shift+M on macOS) first.{readOnly ? '' : ' Ctrl+S (⌘S) saves the draft.'}
          </p>
        </TabsContent>
        <TabsContent value="graph">
          <BuilderView
            source={source}
            onEdit={onBuilderEdit}
            readOnly={readOnly}
            diagnostics={shown}
            stepBudget={stepBudget}
            directory={directory}
            callTargets={callTargets}
            selected={selected}
            onSelect={setSelected}
            history={builderHistory}
            isNew={isNew}
            onShowYaml={showInYaml}
            callHref={callHref}
            {...(triggerUrl ? { triggerUrl } : {})}
            onNavigate={follow}
            viewKey={`editor:${runbookId ?? 'new'}`}
          />
        </TabsContent>
        <TabsContent value="outline">
          {shownOutline ? (
            <>
              {outline ? null : <StaleOutlineNote />}
              <RunbookOutline outline={shownOutline} selectedId={selected} onSelect={setSelected} />
            </>
          ) : (
            <NoOutline />
          )}
        </TabsContent>
      </Tabs>
    </>
  );
}

/** The browser's own unload prompt while there are unsaved edits. */
function useBeforeUnload(dirty: boolean) {
  useEffect(() => {
    if (!dirty) return undefined;
    const warn = (event: BeforeUnloadEvent) => event.preventDefault();
    window.addEventListener('beforeunload', warn);
    return () => window.removeEventListener('beforeunload', warn);
  }, [dirty]);
}

function StaleOutlineNote() {
  return (
    <p className="mb-2 text-sm text-fg-muted">
      The current text does not parse; this is the last outline that did.
    </p>
  );
}

function NoOutline() {
  return (
    <EmptyState
      title="No outline yet"
      description="The source does not parse as a runbook. Fix the YAML errors first."
    />
  );
}

const GAP_TEXT: Record<DirectoryGap, string> = {
  skills:
    'the skill library (your token lacks skills:read), so skill references may show as unknown',
  variables: 'the project variables, so variable references may show as unknown',
};

function DiagnosticsPanel({
  diagnostics,
  fromServer,
  serverRevision,
  compilerError,
  checking,
  gaps,
  contextFailed,
  stepOf,
  onShowStep,
  onShowLine,
}: {
  diagnostics: Diagnostic[];
  fromServer: boolean;
  serverRevision: number | null;
  compilerError: string | null;
  checking: boolean;
  gaps: DirectoryGap[];
  contextFailed: boolean;
  /** The title of the step a diagnostic is in, if any. */
  stepOf(d: Diagnostic): string | undefined;
  onShowStep(d: Diagnostic): void;
  onShowLine(d: Diagnostic): void;
}) {
  const headingId = useId();
  const count = diagnostics.length;
  return (
    <section aria-labelledby={headingId} className="mb-4 grid gap-2" data-testid="diagnostics">
      <h2 id={headingId} className="text-base font-semibold">
        {fromServer ? 'Server diagnostics' : 'Provisional diagnostics'}{' '}
        <span className="font-normal text-fg-muted">({count})</span>
      </h2>
      <p className="text-sm text-fg-muted">
        {fromServer
          ? `From the server for revision ${serverRevision ?? '?'}. These are authoritative.`
          : 'From the browser compiler, as you type. They are provisional: saving or publishing gets the server’s diagnostics, which replace them. Calls to other runbooks are checked only when the server validates.'}
      </p>
      {!fromServer && compilerError ? (
        <p role="alert" className="text-sm">
          Browser checks are unavailable ({compilerError}). Save the draft to get the server’s
          diagnostics.
        </p>
      ) : null}
      {!fromServer && contextFailed ? (
        <p className="text-sm text-fg-muted">
          The directory could not be read, so browser checks are paused. Save the draft to get the
          server’s diagnostics.
        </p>
      ) : null}
      {!fromServer && gaps.length > 0 ? (
        <p className="text-sm text-fg-muted">
          Browser checks cannot read {gaps.map((gap) => GAP_TEXT[gap]).join(', or ')}.
        </p>
      ) : null}
      {count > 0 ? (
        <ul className="grid gap-1 text-sm" aria-label="Problems">
          {diagnostics.map((d, i) => {
            const step = stepOf(d);
            return (
              <li
                key={`${d.code}-${d.path}-${i}`}
                className="flex flex-wrap items-baseline gap-x-2"
              >
                <span className="rounded-sm border border-border px-1 text-xs font-medium uppercase">
                  {fromServer ? 'Server' : 'Provisional'}
                </span>
                <span className="font-mono text-fg-muted">
                  {d.line ? `line ${d.line}${d.column ? `:${d.column}` : ''}` : d.path || '/'}
                </span>
                {/* The human message first, the code as secondary text (#11). */}
                <span>{diagnosticMessage(d)}</span>
                <span className="font-mono text-xs text-fg-muted">{d.code}</span>
                <span className="flex gap-2 text-xs">
                  <button
                    type="button"
                    className="font-medium text-accent hover:underline"
                    onClick={() => onShowStep(d)}
                  >
                    {step ? `Show ${step}` : 'Show in settings'}
                  </button>
                  <button
                    type="button"
                    className="font-medium text-accent hover:underline"
                    onClick={() => onShowLine(d)}
                  >
                    Show in YAML
                  </button>
                </span>
              </li>
            );
          })}
        </ul>
      ) : checking && !fromServer ? (
        <p className="text-sm text-fg-muted">Checking…</p>
      ) : (
        <p className="text-sm">
          {fromServer ? 'The server found no problems.' : 'The browser check found no problems.'}
        </p>
      )}
    </section>
  );
}
