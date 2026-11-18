// UI-4: the builder's form controls. Every control has a visible label, an
// optional hint, and the compiler's problems for its part of the YAML shown
// under it (aria-invalid and aria-describedby point at them). Values come
// from the YAML and every change goes straight back to it.
import { Plus, X } from 'lucide-react';
import { useId, useState, type ReactNode } from 'react';
import { Checkbox } from '../../ui/primitives/checkbox';
import { cn } from '../../ui/primitives/cn';
import { fieldClasses, Input } from '../../ui/primitives/input';
import { Textarea } from '../../ui/primitives/textarea';

export interface FieldProblems {
  /** Messages for this field's part of the YAML (and anything below it). */
  messages: string[];
}

const labelClass = 'text-sm font-medium text-fg';
const hintClass = 'text-xs text-fg-muted';

export function FieldShell({
  id,
  label,
  hint,
  problems,
  children,
  className,
}: {
  id: string;
  label: ReactNode;
  hint?: ReactNode;
  problems?: string[];
  children: (describedBy: string | undefined, invalid: boolean) => ReactNode;
  className?: string;
}) {
  const hintId = `${id}-hint`;
  const errorId = `${id}-error`;
  const invalid = (problems?.length ?? 0) > 0;
  const describedBy =
    [hint ? hintId : null, invalid ? errorId : null].filter(Boolean).join(' ') || undefined;
  return (
    <div className={cn('grid gap-1', className)} data-field={id}>
      <label htmlFor={id} className={labelClass}>
        {label}
      </label>
      {children(describedBy, invalid)}
      {hint ? (
        <p id={hintId} className={hintClass}>
          {hint}
        </p>
      ) : null}
      {invalid ? (
        <ul id={errorId} className="grid gap-0.5 text-xs text-danger-text" data-slot="field-error">
          {problems?.map((message, i) => (
            <li key={i}>{message}</li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

export function TextField({
  label,
  value,
  onChange,
  hint,
  problems,
  placeholder,
  mono = false,
  disabled = false,
  name,
}: {
  label: ReactNode;
  value: string;
  onChange(value: string): void;
  hint?: ReactNode;
  problems?: string[];
  placeholder?: string;
  mono?: boolean;
  disabled?: boolean;
  name?: string;
}) {
  const id = useId();
  return (
    <FieldShell id={id} label={label} hint={hint} problems={problems}>
      {(describedBy, invalid) => (
        <Input
          id={id}
          name={name}
          value={value}
          placeholder={placeholder}
          disabled={disabled}
          autoComplete="off"
          spellCheck={mono ? false : undefined}
          aria-describedby={describedBy}
          aria-invalid={invalid || undefined}
          className={cn('h-9 text-sm', mono && 'font-mono')}
          onChange={(e) => onChange(e.target.value)}
        />
      )}
    </FieldShell>
  );
}

export function TextAreaField({
  label,
  value,
  onChange,
  hint,
  problems,
  rows = 5,
  mono = false,
  disabled = false,
}: {
  label: ReactNode;
  value: string;
  onChange(value: string): void;
  hint?: ReactNode;
  problems?: string[];
  rows?: number;
  mono?: boolean;
  disabled?: boolean;
}) {
  const id = useId();
  return (
    <FieldShell id={id} label={label} hint={hint} problems={problems}>
      {(describedBy, invalid) => (
        <Textarea
          id={id}
          value={value}
          rows={rows}
          disabled={disabled}
          aria-describedby={describedBy}
          aria-invalid={invalid || undefined}
          className={cn('text-sm leading-relaxed', mono && 'font-mono')}
          onChange={(e) => onChange(e.target.value)}
        />
      )}
    </FieldShell>
  );
}

export function NumberField({
  label,
  value,
  onChange,
  hint,
  problems,
  min,
  max,
  placeholder,
}: {
  label: ReactNode;
  value: number | undefined;
  onChange(value: number | undefined): void;
  hint?: ReactNode;
  problems?: string[];
  min?: number;
  max?: number;
  placeholder?: string;
}) {
  const id = useId();
  // Keep what is typed (an empty or half-typed number) until it is a number.
  const [draft, setDraft] = useState<string | null>(null);
  const shown = draft ?? (value === undefined ? '' : String(value));
  return (
    <FieldShell id={id} label={label} hint={hint} problems={problems}>
      {(describedBy, invalid) => (
        <Input
          id={id}
          type="number"
          inputMode="numeric"
          value={shown}
          min={min}
          max={max}
          placeholder={placeholder}
          aria-describedby={describedBy}
          aria-invalid={invalid || undefined}
          className="h-9 text-sm"
          onBlur={() => setDraft(null)}
          onChange={(e) => {
            const text = e.target.value;
            setDraft(text);
            if (text.trim() === '') onChange(undefined);
            else if (Number.isFinite(Number(text))) onChange(Number(text));
          }}
        />
      )}
    </FieldShell>
  );
}

export interface Choice {
  value: string;
  label: string;
}

export function SelectField({
  label,
  value,
  options,
  onChange,
  hint,
  problems,
}: {
  label: ReactNode;
  value: string;
  options: readonly Choice[];
  onChange(value: string): void;
  hint?: ReactNode;
  problems?: string[];
}) {
  const id = useId();
  return (
    <FieldShell id={id} label={label} hint={hint} problems={problems}>
      {(describedBy, invalid) => (
        <select
          id={id}
          value={value}
          aria-describedby={describedBy}
          aria-invalid={invalid || undefined}
          className={cn(fieldClasses, 'h-9 text-sm')}
          onChange={(e) => onChange(e.target.value)}
        >
          {options.map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
      )}
    </FieldShell>
  );
}

export function CheckField({
  label,
  checked,
  onChange,
  hint,
  problems,
  disabled = false,
}: {
  label: ReactNode;
  checked: boolean;
  onChange(checked: boolean): void;
  hint?: ReactNode;
  problems?: string[];
  disabled?: boolean;
}) {
  const id = useId();
  const hintId = `${id}-hint`;
  const errorId = `${id}-error`;
  const invalid = (problems?.length ?? 0) > 0;
  return (
    <div className="grid gap-1" data-field={id}>
      <div className="flex items-start gap-2">
        <Checkbox
          id={id}
          checked={checked}
          disabled={disabled}
          aria-describedby={
            [hint ? hintId : null, invalid ? errorId : null].filter(Boolean).join(' ') || undefined
          }
          aria-invalid={invalid || undefined}
          className="mt-0.5"
          onCheckedChange={(next) => onChange(next === true)}
        />
        <label htmlFor={id} className={cn(labelClass, 'leading-snug')}>
          {label}
        </label>
      </div>
      {hint ? (
        <p id={hintId} className={cn(hintClass, 'pl-7')}>
          {hint}
        </p>
      ) : null}
      {invalid ? (
        <ul id={errorId} className="grid gap-0.5 pl-7 text-xs text-danger-text">
          {problems?.map((message, i) => (
            <li key={i}>{message}</li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

/** A small radio group drawn as segments ("Anyone · Agent · Person"). */
export function SegmentedField({
  label,
  value,
  options,
  onChange,
  hint,
  problems,
}: {
  label: ReactNode;
  value: string;
  options: readonly Choice[];
  onChange(value: string): void;
  hint?: ReactNode;
  problems?: string[];
}) {
  const id = useId();
  const invalid = (problems?.length ?? 0) > 0;
  return (
    <fieldset
      className="grid gap-1"
      aria-describedby={
        [hint ? `${id}-hint` : null, invalid ? `${id}-error` : null].filter(Boolean).join(' ') ||
        undefined
      }
    >
      <legend className={cn(labelClass, 'mb-1')}>{label}</legend>
      <div className="flex rounded-md border border-border-input p-0.5">
        {options.map((option) => (
          <label
            key={option.value}
            className={cn(
              'flex flex-1 cursor-pointer items-center justify-center rounded-sm px-2 py-1 text-sm has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-focus',
              value === option.value
                ? 'bg-accent-subtle font-medium text-accent-strong'
                : 'text-fg-muted hover:bg-hover hover:text-fg',
            )}
          >
            <input
              type="radio"
              name={id}
              value={option.value}
              checked={value === option.value}
              onChange={() => onChange(option.value)}
              className="sr-only"
            />
            {option.label}
          </label>
        ))}
      </div>
      {hint ? (
        <p id={`${id}-hint`} className={hintClass}>
          {hint}
        </p>
      ) : null}
      {invalid ? (
        <ul id={`${id}-error`} className="grid gap-0.5 text-xs text-danger-text">
          {problems?.map((message, i) => (
            <li key={i}>{message}</li>
          ))}
        </ul>
      ) : null}
    </fieldset>
  );
}

/** A list of short strings (checklist items, tags, options) with add and remove. */
export function StringListField({
  label,
  values,
  onChange,
  hint,
  problems,
  itemLabel,
  addLabel,
  mono = false,
  suggestions,
}: {
  label: ReactNode;
  values: string[];
  onChange(values: string[]): void;
  hint?: ReactNode;
  problems?: string[];
  /** The accessible name of item n (1-based). */
  itemLabel: (n: number) => string;
  addLabel: string;
  mono?: boolean;
  /** Offered while typing (a datalist). */
  suggestions?: readonly string[];
}) {
  const id = useId();
  const listId = `${id}-suggestions`;
  const invalid = (problems?.length ?? 0) > 0;
  return (
    <fieldset className="grid gap-1.5">
      <legend className={cn(labelClass, 'mb-1')}>{label}</legend>
      {values.length > 0 ? (
        <ul className="grid gap-1.5">
          {values.map((value, i) => (
            <li key={i} className="flex items-center gap-1.5">
              <Input
                value={value}
                aria-label={itemLabel(i + 1)}
                aria-invalid={invalid || undefined}
                list={suggestions ? listId : undefined}
                autoComplete="off"
                className={cn('h-8 text-sm', mono && 'font-mono')}
                onChange={(e) => onChange(values.map((v, j) => (j === i ? e.target.value : v)))}
              />
              <IconButton
                label={`Remove ${itemLabel(i + 1).toLowerCase()}`}
                onClick={() => onChange(values.filter((_, j) => j !== i))}
              >
                <X className="size-3.5" aria-hidden="true" />
              </IconButton>
            </li>
          ))}
        </ul>
      ) : null}
      {suggestions ? (
        <datalist id={listId}>
          {suggestions.map((s) => (
            <option key={s} value={s} />
          ))}
        </datalist>
      ) : null}
      <div>
        <SmallButton onClick={() => onChange([...values, ''])}>
          <Plus className="size-3.5" aria-hidden="true" />
          {addLabel}
        </SmallButton>
      </div>
      {hint ? <p className={hintClass}>{hint}</p> : null}
      {invalid ? (
        <ul className="grid gap-0.5 text-xs text-danger-text">
          {problems?.map((message, i) => (
            <li key={i}>{message}</li>
          ))}
        </ul>
      ) : null}
    </fieldset>
  );
}

export function IconButton({
  label,
  onClick,
  children,
  disabled = false,
}: {
  label: string;
  onClick(): void;
  children: ReactNode;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      disabled={disabled}
      onClick={onClick}
      className="inline-flex size-8 shrink-0 items-center justify-center rounded-md text-fg-muted hover:bg-hover hover:text-fg focus-visible:outline-2 focus-visible:outline-focus disabled:opacity-40"
    >
      {children}
    </button>
  );
}

export function SmallButton({
  onClick,
  children,
  disabled = false,
  tone = 'default',
}: {
  onClick(): void;
  children: ReactNode;
  disabled?: boolean;
  tone?: 'default' | 'danger';
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className={cn(
        'inline-flex h-8 items-center gap-1.5 rounded-md border border-border px-2.5 text-sm font-medium hover:bg-hover focus-visible:outline-2 focus-visible:outline-focus disabled:opacity-50',
        tone === 'danger' ? 'text-danger-text' : 'text-fg',
      )}
    >
      {children}
    </button>
  );
}

/** A titled group of fields in a panel. */
export function Section({
  title,
  children,
  folded = false,
}: {
  title: string;
  children: ReactNode;
  /** A <details> closed at first (advanced settings). */
  folded?: boolean;
}) {
  const body = <div className="grid gap-3">{children}</div>;
  if (folded) {
    return (
      <details className="group border-t border-border pt-3">
        <summary className="mb-1 cursor-pointer rounded-sm text-xs font-medium uppercase tracking-wider text-fg-muted marker:text-fg-muted hover:text-fg focus-visible:outline-2 focus-visible:outline-focus">
          {title}
        </summary>
        <div className="pt-2">{body}</div>
      </details>
    );
  }
  return (
    <section className="grid gap-2 border-t border-border pt-3 first:border-t-0 first:pt-0">
      <h3 className="text-xs font-medium uppercase tracking-wider text-fg-muted">{title}</h3>
      {body}
    </section>
  );
}
