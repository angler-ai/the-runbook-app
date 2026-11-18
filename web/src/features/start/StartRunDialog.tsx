// Start dialog (M3): the version's inputs/fields JSON Schemas from
// B1.6 rendered with OutputsForm, plus title and tags. The dialog owns the
// drafts and Start validates and sends exactly what the form shows (U9).
// Submission is idempotent (one key per dialog open, reused on retry, D20)
// and a 422 execution.inputs_invalid lands back on the offending fields.
import { Link, useNavigate } from '@tanstack/react-router';
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { ApiError } from '../../api/client';
import { useVersions, versionRef } from '../runbooks/hooks';
import type { RunbookCatalogView } from '../runbooks/hooks';
import {
  fieldKeyOf,
  initialDraft,
  OutputsForm,
  validateDraft,
  type FormDraft,
  type JsonSchema,
  type ProblemError,
} from '../../ui/actions';
import { Button } from '../../ui/primitives/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '../../ui/primitives/dialog';
import { Input } from '../../ui/primitives/input';
import { Label } from '../../ui/primitives/label';
import { useStartRun, useVersionForm } from './hooks';

export interface StartRunDialogProps {
  open: boolean;
  onOpenChange(open: boolean): void;
  projectId: string;
  projectRef: string;
  runbooks: RunbookCatalogView[];
  initialRunbookId?: string;
}

function schemaOf(value: unknown): JsonSchema {
  return value && typeof value === 'object' ? (value as JsonSchema) : {};
}
function hasProperties(schema: JsonSchema): boolean {
  return Boolean(schema.properties && Object.keys(schema.properties).length > 0);
}

/**
 * The runbook the dialog is on: the person's pick, else the one it was
 * opened for, else the first in the catalog. Derived on every render, so a
 * dialog opened while the catalog is still loading resolves once it
 * arrives (R54). A choice missing from a loaded catalog falls back to the
 * first runbook.
 */
export function resolveRunbookId(
  choice: string | undefined,
  runbooks: RunbookCatalogView[],
): string {
  if (runbooks.length === 0) return choice ?? '';
  if (choice && runbooks.some((rb) => rb.runbook_id === choice)) return choice;
  return runbooks[0]?.runbook_id ?? '';
}

/** The drafts of one runbook/version form; another form starts fresh. */
interface Drafts {
  formKey: string;
  inputs: FormDraft;
  fields: FormDraft;
}

type Section = 'inputs' | 'fields';

/** Local validation errors as problem errors under their section's root. */
function asProblems(root: Section, errors: Record<string, string>): ProblemError[] {
  return Object.entries(errors).map(([key, message]) => ({ path: `${root}.${key}`, message }));
}

export function StartRunDialog(props: StartRunDialogProps) {
  // Remount the form on every open so one dialog session is one user
  // action with one idempotency key.
  const [wasOpen, setWasOpen] = useState(props.open);
  const [session, setSession] = useState(0);
  if (props.open !== wasOpen) {
    setWasOpen(props.open);
    if (props.open) setSession((n) => n + 1);
  }
  if (!props.open) {
    return (
      <Dialog open={false} onOpenChange={props.onOpenChange}>
        <DialogContent />
      </Dialog>
    );
  }
  return <StartRunForm key={session} {...props} />;
}

function StartRunForm({
  onOpenChange,
  projectId,
  projectRef,
  runbooks,
  initialRunbookId,
}: StartRunDialogProps) {
  const navigate = useNavigate();
  // Only the person's own choices are state; the selection itself is
  // derived, and the version, inputs and fields are keyed on it.
  const [pickedRunbookId, setPickedRunbookId] = useState<string | undefined>(undefined);
  const runbookId = resolveRunbookId(pickedRunbookId ?? initialRunbookId, runbooks);
  const [versionChoice, setVersionChoice] = useState<{ runbookId: string; version: string }>({
    runbookId: '',
    version: 'latest',
  });
  const version = versionChoice.runbookId === runbookId ? versionChoice.version : 'latest';
  const formKey = `${runbookId}@${version}`;
  const [title, setTitle] = useState('');
  const [tagsText, setTagsText] = useState('');
  const [inputsErrors, setInputsErrors] = useState<ProblemError[]>([]);
  const [fieldsErrors, setFieldsErrors] = useState<ProblemError[]>([]);
  const [formError, setFormError] = useState<ReactNode>(null);
  // One user action: opening the dialog mints the action; every Start
  // attempt from this dialog reuses its idempotency key until success. A
  // rejected start (422) spends no key, so the corrected retry reuses it.
  const [actionId] = useState(() => globalThis.crypto.randomUUID());

  const runbook = runbooks.find((rb) => rb.runbook_id === runbookId);
  const versionsQuery = useVersions(runbookId || undefined);
  const versions = (versionsQuery.data?.pages ?? []).flatMap((page) => page.items);
  const form = useVersionForm(runbookId || undefined, version);
  const start = useStartRun(projectId);

  const inputsSchema = schemaOf(form.data?.inputs);
  const fieldsSchema = schemaOf(form.data?.fields);
  // The drafts are what the forms show; a new runbook or version starts
  // from that version's defaults.
  const [stored, setStored] = useState<Drafts | null>(null);
  const drafts: Drafts =
    stored?.formKey === formKey
      ? stored
      : { formKey, inputs: initialDraft(inputsSchema), fields: initialDraft(fieldsSchema) };
  const setDraft = (section: Section, draft: FormDraft) =>
    setStored({ ...drafts, [section]: draft });
  // "Saved" holds only while the form still shows the saved draft.
  const [saved, setSaved] = useState<Partial<Record<Section, FormDraft>>>({});
  const save = (section: Section) => {
    setStored(drafts);
    setSaved((prev) => ({ ...prev, [section]: drafts[section] }));
    (section === 'inputs' ? setInputsErrors : setFieldsErrors)([]);
  };

  // Move focus to the first marked field after a refused Start.
  const bodyRef = useRef<HTMLDivElement>(null);
  const [focusRequest, setFocusRequest] = useState(0);
  useEffect(() => {
    if (focusRequest === 0) return;
    const invalid = bodyRef.current?.querySelector<HTMLElement>('[aria-invalid="true"]');
    (invalid ?? bodyRef.current?.querySelector<HTMLElement>('[role="alert"]'))?.focus();
  }, [focusRequest]);

  const canStart = Boolean(runbook && form.data && !start.isPending);

  function showFieldErrors(errors: ProblemError[], fallback: string) {
    const known = (root: Section, schema: JsonSchema) => (e: ProblemError) =>
      (e.path ?? '').startsWith(`${root}.`) || (e.path ?? '').startsWith(`${root}[`)
        ? Object.hasOwn(schema.properties ?? {}, fieldKeyOf(e.path ?? ''))
        : false;
    const onInputs = errors.filter(known('inputs', inputsSchema));
    const onFields = errors.filter(known('fields', fieldsSchema));
    const other = errors.filter((e) => !onInputs.includes(e) && !onFields.includes(e));
    setInputsErrors(onInputs);
    setFieldsErrors(onFields);
    setFormError(
      other.length > 0
        ? other.map((e) => (e.path ? `${e.path}: ${e.message}` : e.message)).join(' ')
        : fallback,
    );
    setFocusRequest((n) => n + 1);
  }

  async function onStart() {
    if (!runbook || !canStart) return;
    // Each attempt reports only its own problems.
    setFormError(null);
    setInputsErrors([]);
    setFieldsErrors([]);
    const inputs = validateDraft(inputsSchema, drafts.inputs);
    const fields = validateDraft(fieldsSchema, drafts.fields);
    const local = [...asProblems('inputs', inputs.errors), ...asProblems('fields', fields.errors)];
    if (local.length > 0) {
      showFieldErrors(local, 'Check the marked fields.');
      return;
    }
    try {
      const created = await start.mutateAsync({
        runbook: runbook.slug,
        ...(version === 'latest'
          ? {}
          : { version: /^\d+$/.test(version) ? Number(version) : version }),
        title,
        tags: tagsText
          .split(',')
          .map((tag) => tag.trim())
          .filter(Boolean),
        inputs: inputs.values,
        fields: fields.values,
        actionId,
      });
      onOpenChange(false);
      await navigate({ to: '/x/$exe', params: { exe: created.id } });
    } catch (error) {
      if (error instanceof ApiError && error.code === 'execution.inputs_invalid') {
        showFieldErrors(
          error.errors.map((e) => ({ path: e.path, code: e.code, message: e.message })),
          'Some values do not match this version. Check the marked fields.',
        );
      } else if (error instanceof ApiError && error.code === 'request.idempotency_key_reused') {
        // U21: an earlier Start from this dialog committed (its answer was
        // lost) and the values changed since; the run exists.
        setFormError(
          <>
            This dialog already started a run, with the values it had then. Find it in the{' '}
            <Link
              to="/p/$project/executions"
              params={{ project: projectRef }}
              onClick={() => onOpenChange(false)}
              className="underline"
            >
              executions list
            </Link>
            . To start another run, close this dialog and open it again.
          </>,
        );
        setFocusRequest((n) => n + 1);
      } else if (error instanceof ApiError) {
        setFormError(`The run could not be started (${error.code}).`);
      } else {
        setFormError(
          'The run could not be started. Try again: a retry from this dialog cannot start it twice.',
        );
      }
    }
  }

  return (
    <Dialog open onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Start a run</DialogTitle>
          <DialogDescription>
            Inputs and fields come from the selected version. Retrying reuses the same idempotency
            key, so a retry cannot start the run twice.
          </DialogDescription>
        </DialogHeader>
        <div className="grid gap-4" ref={bodyRef}>
          <div className="grid gap-1">
            <Label htmlFor="start-runbook">Runbook</Label>
            <select
              id="start-runbook"
              value={runbookId}
              onChange={(event) => {
                setPickedRunbookId(event.target.value);
                setVersionChoice({ runbookId: event.target.value, version: 'latest' });
              }}
            >
              {runbooks.map((rb) => (
                <option key={rb.runbook_id} value={rb.runbook_id}>
                  {rb.title ?? rb.slug}
                </option>
              ))}
            </select>
          </div>
          <div className="grid gap-1">
            <Label htmlFor="start-version">Version</Label>
            <select
              id="start-version"
              value={version}
              onChange={(event) => setVersionChoice({ runbookId, version: event.target.value })}
            >
              <option value="latest">Latest published</option>
              {versions.map((v) => (
                <option key={v.runbook_version_id} value={versionRef(v)}>
                  {v.runbook_version != null
                    ? `v${v.runbook_version}`
                    : `preview ${v.runbook_version_id}`}
                </option>
              ))}
            </select>
          </div>
          <div className="grid gap-1">
            <Label htmlFor="start-title">Run title (optional)</Label>
            <Input id="start-title" value={title} onChange={(e) => setTitle(e.target.value)} />
          </div>
          <div className="grid gap-1">
            <Label htmlFor="start-tags">Tags, comma separated (optional)</Label>
            <Input id="start-tags" value={tagsText} onChange={(e) => setTagsText(e.target.value)} />
          </div>
          {form.isLoading ? <p role="status">Loading this version’s form…</p> : null}
          {form.isError ? <p role="alert">This version’s form could not be loaded.</p> : null}
          {form.data && hasProperties(inputsSchema) ? (
            <fieldset className="grid gap-2">
              <legend className="font-medium">Inputs</legend>
              <OutputsForm
                key={`inputs-${formKey}`}
                schema={inputsSchema}
                values={drafts.inputs}
                onValuesChange={(draft) => setDraft('inputs', draft)}
                errors={inputsErrors}
                submitLabel="Save inputs"
                onSubmit={() => save('inputs')}
              />
              <p role="status">
                {saved.inputs !== undefined && saved.inputs === drafts.inputs
                  ? 'Inputs saved.'
                  : null}
              </p>
            </fieldset>
          ) : null}
          {form.data && hasProperties(fieldsSchema) ? (
            <fieldset className="grid gap-2">
              <legend className="font-medium">Fields</legend>
              <OutputsForm
                key={`fields-${formKey}`}
                schema={fieldsSchema}
                values={drafts.fields}
                onValuesChange={(draft) => setDraft('fields', draft)}
                errors={fieldsErrors}
                submitLabel="Save fields"
                onSubmit={() => save('fields')}
              />
              <p role="status">
                {saved.fields !== undefined && saved.fields === drafts.fields
                  ? 'Fields saved.'
                  : null}
              </p>
            </fieldset>
          ) : null}
          {formError ? (
            <p role="alert" tabIndex={-1}>
              {formError}
            </p>
          ) : null}
          <div className="flex gap-2">
            <Button disabled={!canStart} onClick={() => void onStart()}>
              {start.isPending ? 'Starting…' : 'Start run'}
            </Button>
            <Button variant="secondary" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
