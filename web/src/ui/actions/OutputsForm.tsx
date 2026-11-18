import { useState, useId, type FormEvent } from 'react';
import { Button } from '../primitives/button';
import { Checkbox } from '../primitives/checkbox';
import { Input } from '../primitives/input';
import { Label } from '../primitives/label';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '../primitives/select';
import { Textarea } from '../primitives/textarea';
import { cn } from '../primitives/cn';

export interface JsonSchemaProperty {
  type?: string;
  title?: string;
  description?: string;
  default?: unknown;
  enum?: (string | number)[];
  [key: string]: unknown;
}

export interface JsonSchema {
  type?: string;
  properties?: Record<string, JsonSchemaProperty>;
  required?: string[];
  [key: string]: unknown;
}

export interface ProblemError {
  field?: string;
  path?: string;
  message: string;
  code?: string;
}

/**
 * What the form shows, per property: text for typed inputs and JSON
 * editors, a boolean for checkboxes, the given value for unsupported
 * shapes. `validateDraft` turns it into the submitted values.
 */
export type FormDraft = Record<string, unknown>;

export interface OutputsFormProps {
  schema: JsonSchema;
  initialValues?: Record<string, unknown>;
  errors?: ProblemError[];
  onSubmit?: (values: Record<string, unknown>) => void;
  submitLabel?: string;
  disabled?: boolean;
  className?: string;
  /** Controlled draft (see `initialDraft`); the parent then owns the values. */
  values?: FormDraft;
  /** Every edit, with the whole next draft. */
  onValuesChange?: (values: FormDraft) => void;
}

type Kind = 'enum' | 'boolean' | 'integer' | 'number' | 'string' | 'list' | 'object' | 'other';

function kindOf(prop: JsonSchemaProperty): Kind {
  if (prop.enum && prop.enum.length > 0) return 'enum';
  switch (prop.type) {
    case 'boolean':
    case 'integer':
    case 'number':
    case 'string':
      return prop.type;
    case 'array':
      return 'list';
    case 'object':
      return 'object';
    default:
      return 'other';
  }
}

const isJsonKind = (kind: Kind) => kind === 'list' || kind === 'object';

/** The draft a fresh form shows: given values, else schema defaults. */
export function initialDraft(
  schema: JsonSchema,
  initialValues: Record<string, unknown> = {},
): FormDraft {
  const draft: FormDraft = {};
  for (const [key, prop] of Object.entries(schema.properties ?? {})) {
    const given = initialValues[key] !== undefined ? initialValues[key] : prop.default;
    const kind = kindOf(prop);
    if (kind === 'other') {
      // Not editable here: carry the given value through untouched.
      if (given !== undefined) draft[key] = given;
    } else if (isJsonKind(kind)) {
      draft[key] =
        given === undefined || given === null
          ? ''
          : typeof given === 'string'
            ? given
            : JSON.stringify(given, null, 2);
    } else if (kind === 'boolean') {
      draft[key] = given === undefined ? false : given;
    } else {
      draft[key] = given ?? '';
    }
  }
  return draft;
}

/**
 * Validate a draft against the schema. `values` is exactly what is
 * submitted: parsed numbers, parsed JSON, the chosen enum option. Empty
 * optional values are left out so the server applies its defaults.
 */
export function validateDraft(
  schema: JsonSchema,
  draft: FormDraft,
): { values: Record<string, unknown>; errors: Record<string, string> } {
  const required = new Set(schema.required ?? []);
  const values: Record<string, unknown> = {};
  const errors: Record<string, string> = {};
  for (const [key, prop] of Object.entries(schema.properties ?? {})) {
    const val = draft[key];
    const kind = kindOf(prop);
    const isRequired = required.has(key);
    if (kind === 'boolean') {
      // Booleans are always either true or false.
      values[key] = Boolean(val);
      continue;
    }
    if (kind === 'other') {
      if (val === undefined || val === null || val === '') {
        if (isRequired) errors[key] = 'This value cannot be entered in this form.';
      } else {
        values[key] = val;
      }
      continue;
    }
    const strVal = val === undefined || val === null ? '' : String(val).trim();
    if (strVal === '') {
      if (isRequired) errors[key] = 'This field is required';
      continue;
    }
    if (kind === 'integer') {
      // Plain decimal digits only: `1e3`, `0x10` and unsafe magnitudes are refused
      // rather than silently submitted as a different value.
      const num = Number(strVal);
      if (!/^-?\d+$/.test(strVal) || !Number.isSafeInteger(num)) {
        errors[key] = 'Must be an integer';
      } else {
        values[key] = num;
      }
    } else if (kind === 'number') {
      // Decimal notation only, finite: the submitted value is the validated value.
      const num = Number(strVal);
      if (!/^-?(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?$/.test(strVal) || !Number.isFinite(num)) {
        errors[key] = 'Must be a number';
      } else {
        values[key] = num;
      }
    } else if (kind === 'enum') {
      const option = prop.enum?.find((opt) => String(opt) === String(val));
      if (option === undefined) errors[key] = 'Choose one of the options';
      else values[key] = option;
    } else if (isJsonKind(kind)) {
      let parsed: unknown;
      try {
        parsed = JSON.parse(strVal);
      } catch {
        errors[key] = 'Must be valid JSON';
        continue;
      }
      if (kind === 'list' && !Array.isArray(parsed)) {
        errors[key] = 'Must be a JSON list, like ["a", "b"]';
      } else if (
        kind === 'object' &&
        (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed))
      ) {
        errors[key] = 'Must be a JSON object, like {"key": "value"}';
      } else {
        values[key] = parsed;
      }
    } else {
      values[key] = val;
    }
  }
  return { values, errors };
}

/**
 * The top-level property a server error path points at: the root
 * (`inputs.`, `fields.`, `outputs.`) is dropped and nested paths
 * (`regions[1]`, `limits.cpu`, `["odd key"].x`) name their property.
 */
export function fieldKeyOf(path: string): string {
  let rest = path;
  for (const root of ['outputs', 'inputs', 'fields']) {
    if (rest.startsWith(`${root}.`)) {
      rest = rest.slice(root.length + 1);
      break;
    }
    if (rest.startsWith(`${root}[`)) {
      rest = rest.slice(root.length);
      break;
    }
  }
  if (rest.startsWith('[')) {
    const quoted = /^\[("(?:[^"\\]|\\.)*")\]/.exec(rest);
    if (quoted?.[1]) {
      try {
        return JSON.parse(quoted[1]) as string;
      } catch {
        return rest;
      }
    }
    return rest;
  }
  return /^[^.[]+/.exec(rest)?.[0] ?? rest;
}

const NO_ERRORS: ProblemError[] = [];

export function OutputsForm({
  schema,
  initialValues,
  errors = NO_ERRORS,
  onSubmit,
  submitLabel = 'Submit outputs',
  disabled = false,
  className,
  values,
  onValuesChange,
}: OutputsFormProps) {
  const baseId = useId();
  const properties = schema.properties ?? {};
  const requiredFields = new Set(schema.required ?? []);

  const [ownDraft, setOwnDraft] = useState<FormDraft>(() => initialDraft(schema, initialValues));
  const draft = values ?? ownDraft;
  const [localErrors, setLocalErrors] = useState<Record<string, string>>({});
  // Fields edited since the current server errors arrived: their server
  // error no longer describes what the field shows.
  const [edited, setEdited] = useState<{ errors: ProblemError[]; keys: ReadonlySet<string> }>({
    errors,
    keys: new Set(),
  });
  const editedKeys = edited.errors === errors ? edited.keys : new Set<string>();

  const idOf = (key: string) => `${baseId}-${key}`;

  const setValue = (key: string, val: unknown) => {
    const next = { ...draft, [key]: val };
    if (values === undefined) setOwnDraft(next);
    onValuesChange?.(next);
    setEdited({ errors, keys: new Set([...editedKeys, key]) });
    if (localErrors[key]) {
      setLocalErrors((prev) => {
        const rest = { ...prev };
        delete rest[key];
        return rest;
      });
    }
  };

  const getFieldError = (key: string): string | undefined => {
    if (localErrors[key]) return localErrors[key];
    if (editedKeys.has(key)) return undefined;
    return errors.find((e) => fieldKeyOf(e.field ?? e.path ?? '') === key)?.message;
  };

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    const result = validateDraft(schema, draft);
    setLocalErrors(result.errors);
    const firstInvalid = Object.keys(properties).find((key) => result.errors[key]);
    if (firstInvalid !== undefined) {
      document.getElementById(idOf(firstInvalid))?.focus();
      return;
    }
    onSubmit?.(result.values);
  };

  const propEntries = Object.entries(properties);

  if (propEntries.length === 0) {
    return (
      <div className="text-xs text-fg-muted italic p-3 text-center border border-dashed border-border rounded-md">
        No output properties declared.
      </div>
    );
  }

  return (
    <form
      data-slot="outputs-form"
      onSubmit={handleSubmit}
      noValidate
      className={cn('flex flex-col gap-4 w-full', className)}
    >
      {propEntries.map(([key, prop]) => {
        const id = idOf(key);
        const isRequired = requiredFields.has(key);
        const error = getFieldError(key);
        const labelText = prop.title ?? key;
        const kind = kindOf(prop);
        const descriptionId = prop.description ? `${id}-description` : undefined;
        const errorId = error ? `${id}-error` : undefined;
        // U22: the description and error are the field's description; the
        // asterisk is visual, so required is also stated to assistive tech.
        const a11y = {
          'aria-invalid': error ? true : undefined,
          'aria-required': isRequired ? true : undefined,
          'aria-describedby': [descriptionId, errorId].filter(Boolean).join(' ') || undefined,
        };
        const requiredMark = isRequired && (
          <span className="text-danger ml-1" aria-hidden="true">
            *
          </span>
        );
        const description = (indent?: string) =>
          prop.description && (
            <p id={descriptionId} className={cn('text-2xs text-fg-muted', indent)}>
              {prop.description}
            </p>
          );
        const errorText = (indent?: string) =>
          error && (
            <p id={errorId} className={cn('text-2xs text-danger', indent)}>
              {error}
            </p>
          );

        // Enum / Select
        if (kind === 'enum') {
          const raw = draft[key];
          const currentVal = raw !== undefined && raw !== '' ? String(raw) : undefined;

          return (
            <div key={key} className="grid gap-1.5">
              <Label htmlFor={id} className="text-xs font-medium text-fg">
                {labelText}
                {requiredMark}
              </Label>
              {description()}
              <Select
                value={currentVal}
                onValueChange={(v) => setValue(key, v)}
                disabled={disabled}
              >
                <SelectTrigger id={id} {...a11y}>
                  <SelectValue placeholder="Select an option" />
                </SelectTrigger>
                <SelectContent>
                  {(prop.enum ?? []).map((opt) => (
                    <SelectItem key={String(opt)} value={String(opt)}>
                      {String(opt)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {errorText()}
            </div>
          );
        }

        // Boolean checkbox
        if (kind === 'boolean') {
          return (
            <div key={key} className="flex flex-col gap-1">
              <div className="flex items-center gap-3 min-h-[36px]">
                <Checkbox
                  id={id}
                  checked={Boolean(draft[key])}
                  disabled={disabled}
                  onCheckedChange={(checked) => setValue(key, Boolean(checked))}
                  {...a11y}
                />
                <Label htmlFor={id} className="text-xs font-medium text-fg cursor-pointer">
                  {labelText}
                  {requiredMark}
                </Label>
              </div>
              {description('pl-7')}
              {errorText('pl-7')}
            </div>
          );
        }

        // String, Number, Integer
        if (kind === 'string' || kind === 'number' || kind === 'integer') {
          const raw = draft[key];
          const valStr = raw !== undefined && raw !== null ? String(raw) : '';

          return (
            <div key={key} className="grid gap-1.5">
              <Label htmlFor={id} className="text-xs font-medium text-fg">
                {labelText}
                {requiredMark}
              </Label>
              {description()}
              <Input
                id={id}
                type="text"
                inputMode={kind === 'integer' ? 'numeric' : kind === 'number' ? 'decimal' : 'text'}
                value={valStr}
                disabled={disabled}
                onChange={(e) => setValue(key, e.target.value)}
                {...a11y}
              />
              {errorText()}
            </div>
          );
        }

        // List and object: edited as JSON (U24), parsed and kind-checked on submit.
        if (isJsonKind(kind)) {
          const hintId = `${id}-hint`;
          return (
            <div key={key} className="grid gap-1.5">
              <Label htmlFor={id} className="text-xs font-medium text-fg">
                {labelText}
                {requiredMark}
              </Label>
              {description()}
              <Textarea
                id={id}
                value={String(draft[key] ?? '')}
                disabled={disabled}
                spellCheck={false}
                onChange={(e) => setValue(key, e.target.value)}
                className="font-mono text-xs"
                {...a11y}
                aria-describedby={[descriptionId, hintId, errorId].filter(Boolean).join(' ')}
              />
              <p id={hintId} className="text-2xs text-fg-muted">
                {kind === 'list'
                  ? 'A JSON list, like ["a", "b"].'
                  : 'A JSON object, like {"key": "value"}.'}
              </p>
              {errorText()}
            </div>
          );
        }

        // Unsupported schema type: its given value is kept and submitted as is.
        return (
          <div
            key={key}
            className="grid gap-1.5 p-3 rounded-md bg-surface-raised border border-border"
          >
            <div className="text-xs font-medium text-fg">
              {labelText}
              {requiredMark}
            </div>
            <p id={id} tabIndex={-1} className="text-2xs text-fg-muted italic">
              Type "{prop.type ?? 'unknown'}" is not supported in this form.
              {draft[key] !== undefined ? ' Its current value is kept.' : ''}
            </p>
            {errorText()}
          </div>
        );
      })}

      {onSubmit ? (
        <div className="pt-2">
          <Button type="submit" disabled={disabled}>
            {submitLabel}
          </Button>
        </div>
      ) : null}
    </form>
  );
}
