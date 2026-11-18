// UI-2: text with `${{ … }}` templates, each shown as a subtle inline
// monospace chip holding just the expression (`${{ region }}` → `region`).
import { Fragment } from 'react';
import { cn } from '../primitives/cn';

export type TemplatePart = { kind: 'text'; text: string } | { kind: 'expr'; text: string };

const TEMPLATE = /\$\{\{([\s\S]*?)\}\}/g;

/** Split `text` into literal runs and template expressions, in order. */
export function templateParts(text: string): TemplatePart[] {
  const parts: TemplatePart[] = [];
  let last = 0;
  for (const match of text.matchAll(TEMPLATE)) {
    const at = match.index ?? 0;
    if (at > last) parts.push({ kind: 'text', text: text.slice(last, at) });
    parts.push({ kind: 'expr', text: (match[1] ?? '').trim() });
    last = at + match[0].length;
  }
  if (last < text.length) parts.push({ kind: 'text', text: text.slice(last) });
  return parts;
}

/** The text with each template replaced by its bare expression. */
export function plainTemplateText(text: string): string {
  return templateParts(text)
    .map((p) => p.text)
    .join('');
}

export interface TemplateTextProps {
  text: string;
  className?: string;
  chipClassName?: string;
}

export function TemplateText({ text, className, chipClassName }: TemplateTextProps) {
  const parts = templateParts(text);
  return (
    <span className={className}>
      {parts.map((part, i) =>
        part.kind === 'text' ? (
          <Fragment key={i}>{part.text}</Fragment>
        ) : (
          <code
            key={i}
            data-slot="template-chip"
            title={`\${{ ${part.text} }}`}
            className={cn(
              'mx-px rounded-sm border border-accent-line bg-accent-subtle px-1 py-px font-mono text-[0.86em] font-medium text-accent-strong',
              chipClassName,
            )}
          >
            {part.text}
          </code>
        ),
      )}
    </span>
  );
}
