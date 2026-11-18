// UI-4: how a runbook starts. Blank (only a name; the ID comes from it and
// can be changed), from a template, or by an AI agent (UI-1's prompt and
// commands; the agent's draft then appears in the list). Choosing opens the
// builder with an unsaved draft; nothing is created until it is saved.
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import { ArrowRight, Bot, FilePlus2, LayoutTemplate } from 'lucide-react';
import { useEffect, useId, useState, type ReactNode } from 'react';
import { queryKeys } from '../../api/query-keys';
import { Button } from '../../ui/primitives/button';
import { cn } from '../../ui/primitives/cn';
import { Input } from '../../ui/primitives/input';
import { slugify } from '../runbooks/catalog';
import { findRunbookBySlug, useRunbookCatalog } from '../runbooks/hooks';
import { AgentCreatePanel } from '../runbooks/RunbooksEmpty';
import { paletteEntry } from './kinds';
import { RUNBOOK_ID } from './ids';
import { TEMPLATES, type TemplateId } from './templates';

type Way = 'blank' | 'template' | 'agent';

export interface NewRunbookFormProps {
  projectId: string;
  projectRef: string;
  /** Offer "With your AI agent" as a third way (the dialog); the empty state shows it beside. */
  withAgent?: boolean;
  /** Called once the builder opens (the dialog closes). */
  onDone?: () => void;
  origin?: string;
}

export function NewRunbookForm({
  projectId,
  projectRef,
  withAgent = false,
  onDone,
  origin = window.location.origin,
}: NewRunbookFormProps) {
  const navigate = useNavigate();
  const catalog = useRunbookCatalog(projectId);
  const [way, setWay] = useState<Way>('blank');
  const [template, setTemplate] = useState<TemplateId>('post-deploy');
  const [name, setName] = useState('');
  const [id, setId] = useState('');
  const [idEdited, setIdEdited] = useState(false);
  const [tried, setTried] = useState(false);
  const ids = { name: useId(), id: useId(), idHint: useId(), error: useId(), ways: useId() };

  const effectiveId = idEdited ? id : slugify(name);
  const wellFormed = RUNBOOK_ID.test(effectiveId) && effectiveId.length <= 63;
  // Review round 1: whether the ID is taken is read for that ID itself
  // (the catalogue's loaded pages may not hold it), a moment after typing
  // stops, and again on Continue. UI-1c: by the runbook's own route, not a
  // walk of the list.
  const queryClient = useQueryClient();
  const slugQuery = (slug: string) => ({
    queryKey: [...queryKeys.runbooks(projectId), 'slug', slug],
    queryFn: ({ signal }: { signal: AbortSignal }) => findRunbookBySlug(projectId, slug, signal),
    staleTime: 30_000,
  });
  const [checkId, setCheckId] = useState('');
  useEffect(() => {
    const timer = setTimeout(() => setCheckId(effectiveId), 300);
    return () => clearTimeout(timer);
  }, [effectiveId]);
  const slugCheck = useQuery({
    ...slugQuery(checkId),
    enabled: checkId !== '' && checkId !== 'new' && RUNBOOK_ID.test(checkId),
  });
  const taken =
    (catalog.data?.pages ?? []).flatMap((p) => p.items).some((row) => row.slug === effectiveId) ||
    (checkId === effectiveId && slugCheck.data != null);
  const problem =
    name.trim() === ''
      ? 'Give the runbook a name.'
      : effectiveId === ''
        ? 'The ID needs at least one letter or digit.'
        : !wellFormed
          ? 'The ID takes lowercase letters, digits and single hyphens (my-runbook).'
          : effectiveId === 'new'
            ? '“new” is reserved for the new-runbook page; pick another ID.'
            : taken
              ? 'A runbook with this ID already exists in this project.'
              : null;

  async function onContinue() {
    setTried(true);
    if (problem) return;
    // An unanswered read does not block: the server refuses a taken slug on save.
    const existing = await queryClient.fetchQuery(slugQuery(effectiveId)).catch(() => null);
    if (existing) {
      setCheckId(effectiveId);
      return;
    }
    void navigate({
      to: '/p/$project/runbooks/new',
      params: { project: projectRef },
      search: {
        template: way === 'template' ? template : 'blank',
        name: name.trim(),
        id: effectiveId,
      },
    });
    onDone?.();
  }

  function pickTemplate(next: TemplateId) {
    const before = TEMPLATES.find((t) => t.id === template)?.title;
    setTemplate(next);
    // A name still equal to the old template's title follows the new one.
    if (name === '' || name === before) setName(TEMPLATES.find((t) => t.id === next)?.title ?? '');
  }

  const ways: { value: Way; label: string; icon: ReactNode; hint: string }[] = [
    {
      value: 'blank',
      label: 'Start blank',
      icon: <FilePlus2 className="size-4" aria-hidden="true" />,
      hint: 'Only a name; add steps in the builder.',
    },
    {
      value: 'template',
      label: 'From a template',
      icon: <LayoutTemplate className="size-4" aria-hidden="true" />,
      hint: 'Post-deploy checks, customer onboarding.',
    },
    ...(withAgent
      ? [
          {
            value: 'agent' as const,
            label: 'With your AI agent',
            icon: <Bot className="size-4" aria-hidden="true" />,
            hint: 'It writes and saves the draft.',
          },
        ]
      : []),
  ];

  return (
    <form
      aria-label="New runbook"
      className="grid gap-4"
      noValidate
      onSubmit={(event) => {
        event.preventDefault();
        void onContinue();
      }}
    >
      <fieldset className="grid gap-2">
        <legend className="mb-1 text-sm font-medium text-fg">How to start</legend>
        <div className={cn('grid gap-2', withAgent ? 'sm:grid-cols-3' : 'sm:grid-cols-2')}>
          {ways.map((option) => (
            <label
              key={option.value}
              className={cn(
                'grid cursor-pointer gap-1 rounded-md border p-3 text-sm has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-focus',
                way === option.value
                  ? 'border-accent bg-accent-subtle'
                  : 'border-border hover:bg-hover',
              )}
            >
              <span className="flex items-center gap-2 font-medium text-fg">
                <input
                  type="radio"
                  name={ids.ways}
                  value={option.value}
                  aria-labelledby={`${ids.ways}-${option.value}-label`}
                  aria-describedby={`${ids.ways}-${option.value}-hint`}
                  checked={way === option.value}
                  onChange={() => {
                    setWay(option.value);
                    if (option.value === 'template' && name === '')
                      setName(TEMPLATES.find((t) => t.id === template)?.title ?? '');
                  }}
                  className="sr-only"
                />
                <span className={way === option.value ? 'text-accent' : 'text-fg-muted'}>
                  {option.icon}
                </span>
                <span id={`${ids.ways}-${option.value}-label`}>{option.label}</span>
              </span>
              <span id={`${ids.ways}-${option.value}-hint`} className="text-xs text-fg-muted">
                {option.hint}
              </span>
            </label>
          ))}
        </div>
      </fieldset>

      {way === 'agent' ? (
        <div className="grid gap-3">
          <AgentCreatePanel origin={origin} project={projectRef} />
          <p className="text-sm text-fg-muted">
            The agent’s draft appears in the runbook list; open it there to review and publish.
          </p>
          <div className="flex justify-end">
            <Button type="button" variant="secondary" onClick={() => onDone?.()}>
              Done
            </Button>
          </div>
        </div>
      ) : (
        <>
          {way === 'template' ? (
            <fieldset className="grid gap-2">
              <legend className="mb-1 text-sm font-medium text-fg">Template</legend>
              <div className="grid gap-2">
                {TEMPLATES.filter((t) => t.id !== 'blank').map((t) => (
                  <div
                    key={t.id}
                    data-template={t.id}
                    className={cn(
                      'grid gap-1 rounded-md border p-3 has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-focus',
                      template === t.id
                        ? 'border-accent bg-accent-subtle'
                        : 'border-border hover:bg-hover',
                    )}
                  >
                    <span className="flex items-center justify-between gap-2">
                      <span className="flex items-center gap-2 text-sm font-medium text-fg">
                        <input
                          id={`${ids.ways}-t-${t.id}-radio`}
                          type="radio"
                          name={`${ids.ways}-template`}
                          value={t.id}
                          aria-describedby={`${ids.ways}-t-${t.id}-text`}
                          checked={template === t.id}
                          onChange={() => pickTemplate(t.id)}
                          className="accent-[var(--color-accent)]"
                        />
                        <label htmlFor={`${ids.ways}-t-${t.id}-radio`} className="cursor-pointer">
                          {t.title}
                        </label>
                      </span>
                      <span className="flex items-center gap-1 text-fg-muted" aria-hidden="true">
                        {t.kinds.map((kind) => {
                          const Icon = paletteEntry(kind).icon;
                          return <Icon key={kind} className="size-3.5" />;
                        })}
                      </span>
                    </span>
                    <span id={`${ids.ways}-t-${t.id}-text`} className="grid gap-1 pl-6">
                      <span className="text-xs text-fg-muted">{t.description}</span>
                      <span className="text-2xs font-medium uppercase tracking-wider text-fg-muted">
                        {t.who}
                      </span>
                    </span>
                  </div>
                ))}
              </div>
            </fieldset>
          ) : null}
          <div className="grid gap-1.5">
            <label htmlFor={ids.name} className="text-sm font-medium text-fg">
              Name
            </label>
            <Input
              id={ids.name}
              value={name}
              placeholder="Post-deploy checks"
              autoComplete="off"
              aria-invalid={tried && problem === 'Give the runbook a name.' ? true : undefined}
              aria-describedby={tried && problem ? ids.error : undefined}
              onChange={(e) => setName(e.target.value)}
            />
          </div>
          <div className="grid gap-1.5">
            <label htmlFor={ids.id} className="text-sm font-medium text-fg">
              ID
            </label>
            <Input
              id={ids.id}
              value={effectiveId}
              placeholder="post-deploy-checks"
              autoComplete="off"
              spellCheck={false}
              className="font-mono text-sm"
              aria-describedby={`${ids.idHint}${tried && problem ? ` ${ids.error}` : ''}`}
              aria-invalid={
                tried && problem && problem !== 'Give the runbook a name.' ? true : undefined
              }
              onChange={(e) => {
                setIdEdited(true);
                setId(e.target.value);
              }}
            />
            <p id={ids.idHint} className="text-xs text-fg-muted">
              From the name. It names the runbook in URLs, the CLI and calls, and is fixed once
              saved.
            </p>
          </div>
          {tried && problem ? (
            <p id={ids.error} role="alert" className="text-sm text-danger-text">
              {problem}
            </p>
          ) : null}
          <p className="text-xs text-fg-muted">
            The builder opens with an unsaved draft. Saving creates it; publishing is separate. To
            import YAML, start blank and paste it in the YAML tab.
          </p>
          <div className="flex justify-end gap-2">
            {onDone ? (
              <Button type="button" variant="secondary" onClick={() => onDone()}>
                Cancel
              </Button>
            ) : null}
            <Button type="submit">
              Continue to the builder
              <ArrowRight aria-hidden="true" />
            </Button>
          </div>
        </>
      )}
    </form>
  );
}
