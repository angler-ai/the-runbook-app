import {
  useEffect,
  useId,
  useRef,
  useState,
  type ComponentProps,
  type FormEvent,
  type ReactNode,
} from 'react';
import { Plus, X } from 'lucide-react';
import { Button } from '../../ui/primitives/button';
import { Input, fieldClasses } from '../../ui/primitives/input';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogTitle,
} from '../../ui/primitives/dialog';
import { runnerError } from './api';
import { Field, panel } from './presentation';
import type { EgressRule, Limits, Project, Share } from './resources';

export const selectClasses = fieldClasses + ' h-10';
export const textLink = 'font-medium text-accent underline-offset-4 hover:underline';

export function Problem({ children }: { children?: ReactNode }) {
  return children ? (
    <p
      role="alert"
      className="rounded-md border border-border bg-surface px-3 py-2 text-sm text-danger"
    >
      {children}
    </p>
  ) : null;
}

type FieldIssue = { name: string; message: string; index?: number };

export class FieldError extends Error {
  issues: FieldIssue[];

  constructor(name: string | FieldIssue[], message = '', index?: number) {
    const issues = typeof name === 'string' ? [{ name, message, index }] : name;
    super(issues.map((issue) => issue.message).join(' '));
    this.issues = issues;
  }
}

export function RunnerForm({
  onSubmit,
  ...props
}: Omit<ComponentProps<'form'>, 'onSubmit'> & {
  onSubmit(event: FormEvent<HTMLFormElement>): void | Promise<void>;
}) {
  const id = useId();
  const form = useRef<HTMLFormElement>(null);
  const [error, setError] = useState<FieldError>();
  const [fallback, setFallback] = useState('');
  const fields = useRef<(HTMLElement | undefined)[]>([]);
  const focusField = useRef<(control?: HTMLElement) => void>(() => {});
  useEffect(() => {
    if (!error || !form.current) return;
    function focus(control?: HTMLElement) {
      for (
        let parent = control?.parentElement;
        parent && parent !== form.current;
        parent = parent.parentElement
      ) {
        if (parent instanceof HTMLDetailsElement) parent.open = true;
      }
      control?.focus();
    }
    focusField.current = focus;
    const controls = Array.from(form.current.elements).filter(
      (element): element is HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement =>
        element instanceof HTMLInputElement ||
        element instanceof HTMLSelectElement ||
        element instanceof HTMLTextAreaElement,
    );
    const cleanup = error.issues.map(({ name, message, index = 0 }, position) => {
      const control = controls.filter((element) => element.name === name)[index];
      if (!control || (control instanceof HTMLInputElement && control.type === 'hidden')) return;
      const describedBy = control.getAttribute('aria-describedby');
      const invalid = control.getAttribute('aria-invalid');
      const notice = document.createElement('span');
      notice.id = `${id}-error-${position}`;
      notice.className = 'block text-xs text-danger';
      notice.textContent = message;
      // Place the message beside the control without replacing the label or existing help.
      (control.labels?.[0] ?? control).insertAdjacentElement('afterend', notice);
      control.setAttribute('aria-describedby', [describedBy, notice.id].filter(Boolean).join(' '));
      control.setAttribute('aria-invalid', 'true');
      fields.current[position] = control;
      return () => {
        notice.remove();
        if (describedBy === null) control.removeAttribute('aria-describedby');
        else control.setAttribute('aria-describedby', describedBy);
        if (invalid === null) control.removeAttribute('aria-invalid');
        else control.setAttribute('aria-invalid', invalid);
      };
    });
    focus(
      controls.find((control) => fields.current.includes(control) && !control.matches(':disabled')),
    );
    return () => {
      fields.current = [];
      cleanup.reverse().forEach((clear) => clear?.());
    };
  }, [error, id]);
  return (
    <form
      {...props}
      ref={form}
      onSubmit={async (event) => {
        event.preventDefault();
        setError(undefined);
        setFallback('');
        try {
          await onSubmit(event);
        } catch (problem) {
          if (problem instanceof FieldError) setError(problem);
          else setFallback(runnerError(problem));
        }
      }}
    >
      {error && (
        <Problem>
          {error.issues.map(({ message }, position) => (
            <button
              key={position}
              type="button"
              className="block text-left underline"
              onClick={() => focusField.current(fields.current[position])}
            >
              {message}
            </button>
          ))}
        </Problem>
      )}
      {props.children}
      <Problem>{fallback}</Problem>
    </form>
  );
}

export function ReviewChanges({
  title,
  description,
  confirm,
  children,
  close,
  onConfirm,
  pending,
}: {
  title: string;
  description: string;
  confirm: string;
  children: ReactNode;
  close(): void;
  onConfirm(): void | Promise<void>;
  pending?: boolean;
}) {
  const opener = useRef(typeof document !== 'undefined' ? document.activeElement : null);
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !pending) close();
      }}
    >
      <DialogContent
        showClose={false}
        onCloseAutoFocus={(event) => {
          const element = opener.current;
          if (element instanceof HTMLElement && element.isConnected) {
            event.preventDefault();
            element.focus();
          }
        }}
      >
        <DialogTitle>{title}</DialogTitle>
        <DialogDescription>{description}</DialogDescription>
        {children}
        <DialogFooter>
          <Button type="button" variant="secondary" disabled={pending} onClick={close}>
            Cancel
          </Button>
          <Button type="button" disabled={pending} onClick={() => void onConfirm()}>
            {confirm}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export type CapChange = { scope: string; label: string; before: string; after: string | null };

export function increasedCaps(scope: string, previous: Limits, next: Limits): CapChange[] {
  return budgetFields.flatMap(([key, label]) => {
    const before = previous[key];
    const after = next[key];
    if (
      key === 'alert_threshold_percent' ||
      before == null ||
      (after != null && Number(after) <= Number(before))
    )
      return [];
    const format = (value: number) => (key === 'concurrency' ? String(value) : `$${value}`);
    return [
      {
        scope,
        label: label.replace(' (USD)', ''),
        before: format(before),
        after: after == null ? null : format(after),
      },
    ];
  });
}

export function ReviewBudgetChanges({
  changes,
  close,
  onConfirm,
  pending,
}: {
  changes: CapChange[];
  close(): void;
  onConfirm(): void | Promise<void>;
  pending?: boolean;
}) {
  return (
    <ReviewChanges
      title="Review budget increases"
      description="Runner work can use more at the scopes below. Other applicable caps still apply."
      confirm={
        changes.every((change) => change.after === null)
          ? changes.length === 1
            ? `Remove ${changes[0]!.label.toLowerCase()}`
            : 'Remove caps'
          : 'Confirm budget changes'
      }
      close={close}
      onConfirm={onConfirm}
      pending={pending}
    >
      <ul className="grid gap-2 text-sm">
        {changes.map(({ scope, label, before, after }) => (
          <li key={`${scope}:${label}`}>
            <strong>
              {scope} · {label}
            </strong>
            <span className="block text-fg-muted">
              From {before} to {after ?? 'no cap at this scope'}
            </span>
          </li>
        ))}
      </ul>
    </ReviewChanges>
  );
}

export function FormSection({
  title,
  description,
  children,
  id,
}: {
  title: string;
  description?: string;
  children: ReactNode;
  id?: string;
}) {
  return (
    <section id={id} className={panel + ' grid min-w-0 gap-4 sm:p-5'}>
      <div>
        <h2 className="font-semibold">{title}</h2>
        {description && <p className="mt-1 text-sm text-fg-muted">{description}</p>}
      </div>
      {children}
    </section>
  );
}

export function ShareFields({
  projects,
  initial,
  name = 'sharing',
}: {
  projects: Project[];
  initial?: Share;
  name?: string;
}) {
  const selection = initial?.projects ?? 'none';
  const [mode, setMode] = useState(Array.isArray(selection) ? 'selected' : selection);
  return (
    <div className="grid gap-3">
      <Field label="Project access">
        <select
          name={name}
          className={selectClasses}
          value={mode}
          onChange={(event) => setMode(event.target.value)}
        >
          <option value="none">Private to organization admins</option>
          <option value="all">All projects</option>
          <option value="selected">Selected projects</option>
        </select>
      </Field>
      {mode === 'selected' && (
        <fieldset className="grid gap-2 rounded-md border border-border p-3">
          <legend className="px-1 text-sm font-medium">Projects</legend>
          {projects.map((project) => (
            <label key={project.id} className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                className="accent-accent"
                name={name + ':projects'}
                value={project.id}
                defaultChecked={Array.isArray(selection) && selection.includes(project.id)}
              />
              {project.title}
            </label>
          ))}
          {!projects.length && (
            <p className="text-sm text-fg-muted">There are no available projects.</p>
          )}
        </fieldset>
      )}
    </div>
  );
}

export function readShare(
  data: FormData,
  name = 'sharing',
  initial?: Share,
  visibleProjects: Project[] = [],
): Share {
  const value = data.get(name);
  if (value !== 'selected') return { projects: value === 'all' ? 'all' : 'none' };
  const selected = data.getAll(name + ':projects').map(String);
  const retained = Array.isArray(initial?.projects)
    ? initial.projects.filter((id) => !visibleProjects.some((project) => project.id === id))
    : [];
  const projects = [...selected, ...retained];
  return { projects: projects.length ? projects : 'none' };
}

export function readText(data: FormData, name: string) {
  return String(data.get(name) ?? '').trim();
}

export function requiredText(data: FormData, name: string, label: string, max = 128) {
  const value = readText(data, name);
  if (!value || value.length > max)
    throw new FieldError(name, `${label} must contain 1–${max} characters.`);
  return value;
}

export function endpoint(
  value: string,
  label: string,
  options: { query?: boolean; http?: boolean; field?: string } = {},
) {
  if (value.length <= 2048 && URL.canParse(value)) {
    const url = new URL(value);
    if (
      (url.protocol === 'https:' || (options.http && url.protocol === 'http:')) &&
      !url.username &&
      !url.password &&
      !url.hash &&
      (options.query || !url.search)
    )
      return url.href;
  }
  throw new FieldError(
    options.field ?? 'url',
    `${label} must be an ${options.http ? 'HTTP or HTTPS' : 'HTTPS'} URL without credentials, a fragment${options.query ? '' : ', or a query string'}.`,
  );
}

export function numberField(
  data: FormData,
  name: string,
  label: string,
  { min = 0, max = 99_999_999.99, integer = false, required = false } = {},
) {
  const raw = readText(data, name);
  if (!raw && !required) return null;
  const value = Number(raw);
  if (
    !raw ||
    !Number.isFinite(value) ||
    value < min ||
    value > max ||
    (integer && !Number.isInteger(value))
  )
    throw new FieldError(
      name,
      `${label} must be ${integer ? 'a whole number' : 'a number'} between ${min} and ${max}.`,
    );
  return value;
}

export const budgetFields = [
  ['cost_per_day_usd', 'Daily cap (USD)'],
  ['cost_per_month_usd', 'Monthly cap (USD)'],
  ['concurrency', 'Concurrent steps'],
  ['max_step_cost_usd', 'Default step cap (USD)'],
  ['max_run_cost_usd', 'Default run cap (USD)'],
  ['alert_threshold_percent', 'Alert threshold (%)'],
] as const;

export function BudgetFields({
  initial,
  prefix = '',
  keys = budgetFields.map(([key]) => key),
}: {
  initial?: Limits;
  prefix?: string;
  keys?: (keyof Limits)[];
}) {
  return (
    <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
      {budgetFields
        .filter(([key]) => keys.includes(key))
        .map(([key, label]) => (
          <Field key={key} label={label}>
            <Input
              inputMode={key === 'concurrency' ? 'numeric' : 'decimal'}
              name={prefix + key}
              defaultValue={initial?.[key] ?? ''}
              placeholder="No cap"
            />
          </Field>
        ))}
    </div>
  );
}

export function readBudgets(
  data: FormData,
  prefix = '',
  keys = budgetFields.map(([key]) => key),
  scope = '',
) {
  const issues: FieldIssue[] = [];
  const limits = Object.fromEntries(
    budgetFields
      .filter(([key]) => keys.includes(key))
      .map(([key, label]) => {
        try {
          return [
            key,
            numberField(
              data,
              prefix + key,
              scope ? `${scope} ${label.toLowerCase()}` : label,
              key === 'concurrency'
                ? { integer: true, max: 2_147_483_647 }
                : key === 'alert_threshold_percent'
                  ? { min: 1, max: 100 }
                  : {},
            ),
          ];
        } catch (error) {
          if (!(error instanceof FieldError)) throw error;
          issues.push(...error.issues);
          return [key, null];
        }
      }),
  ) as Limits;
  if (issues.length) throw new FieldError(issues);
  return limits;
}

export function EgressFields({
  initial = [],
  name = 'egress',
}: {
  initial?: EgressRule[];
  name?: string;
}) {
  const id = useId();
  const [rows, setRows] = useState(() => initial.map((rule, i) => ({ ...rule, key: String(i) })));
  return (
    <div className="grid gap-2">
      {rows.map((rule, index) => (
        <div key={rule.key} className="grid grid-cols-[minmax(0,1fr)_6rem_auto] items-end gap-2">
          <Field label={index === 0 ? 'Host' : 'Host ' + (index + 1)}>
            <Input name={name + ':host'} defaultValue={rule.host} placeholder="api.example.com" />
          </Field>
          <Field label="Access">
            <select name={name + ':label'} className={selectClasses} defaultValue={rule.label}>
              <option value="read">Read</option>
              <option value="write">Write</option>
            </select>
          </Field>
          <Button
            type="button"
            variant="secondary"
            size="icon"
            aria-label={`Remove host ${index + 1}`}
            onClick={() => setRows(rows.filter((row) => row.key !== rule.key))}
          >
            <X aria-hidden />
          </Button>
        </div>
      ))}
      <Button
        type="button"
        variant="secondary"
        size="sm"
        className="w-fit"
        onClick={() =>
          setRows([...rows, { key: id + crypto.randomUUID(), host: '', label: 'read' }])
        }
      >
        <Plus aria-hidden /> Add host
      </Button>
    </div>
  );
}

export function readEgress(data: FormData, name = 'egress'): EgressRule[] {
  const labels = data.getAll(name + ':label');
  return data.getAll(name + ':host').map((value, index) => {
    const host = String(value).trim().toLowerCase();
    if (
      host.length > 253 ||
      !host.split('.').every((part) => /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(part))
    )
      throw new FieldError(
        name + ':host',
        `Host ${index + 1} must be a hostname without a protocol, port, or path.`,
        index,
      );
    return { host, label: labels[index] === 'write' ? 'write' : 'read' };
  });
}
