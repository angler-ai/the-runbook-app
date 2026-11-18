import type { ReactNode } from 'react';
import type { components } from '../../api/schema';
import { cn } from '../primitives/cn';
import { MonoRef } from '../status/MonoRef';
import { Button } from '../primitives/button';
import { Sparkles, GitBranch, FileCode, History, BookOpen, Upload } from 'lucide-react';

export type SkillVersionView = components['schemas']['SkillVersionView'];

export interface SkillHistoryItem {
  version: string;
  hash: string;
  /** The version runbooks pin when they name this skill. */
  latest?: boolean;
}

type SkillUsageView = SkillVersionView['usages'][number];

/** How a usage reads: the runbook's title (or a link to it) and its version label. */
export interface SkillUsageLabel {
  runbook: ReactNode;
  version?: string;
}

export interface SkillDetailProps {
  skill: SkillVersionView;
  skillMarkdown?: string;
  history?: SkillHistoryItem[];
  onOpenFile?: (path: string) => void;
  className?: string;
  onNewVersion?: () => void;
  /** Names a usage's runbook and version; without it the IDs show. */
  describeUsage?: (usage: SkillUsageView) => SkillUsageLabel;
}

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function SkillDetail({
  skill,
  describeUsage,
  skillMarkdown,
  history,
  onOpenFile,
  className,
  onNewVersion,
}: SkillDetailProps) {
  const provenance = skill.reference || 'local';

  return (
    <div data-slot="skill-detail" className={cn('flex flex-col gap-6 text-xs text-fg', className)}>
      {/* Header */}
      <div className="flex flex-col gap-2 pb-4 border-b border-border">
        <div className="flex items-center justify-between gap-3 flex-wrap">
          <div className="flex min-w-0 items-center gap-2.5">
            <div className="size-8 shrink-0 rounded-md flex items-center justify-center bg-accent/10 border border-accent/20 text-accent">
              <Sparkles className="size-4" aria-hidden="true" />
            </div>
            <div className="min-w-0">
              <h3 className="text-sm font-semibold break-words text-fg">{skill.name}</h3>
              <div className="flex min-w-0 flex-wrap items-center gap-2 mt-0.5">
                <span
                  data-slot="provenance-tag"
                  className="inline-flex items-center gap-1 text-[10px] font-mono px-1.5 py-0.5 rounded bg-surface-raised border border-border text-fg-muted"
                >
                  <GitBranch className="size-2.5 shrink-0" aria-hidden="true" />
                  <span>{provenance}</span>
                </span>
                <span className="min-w-0 text-[10px] break-all text-fg-muted">
                  Digest: <MonoRef>{skill.hash}</MonoRef>
                </span>
              </div>
            </div>
          </div>
          {onNewVersion ? (
            <Button size="sm" variant="secondary" onClick={onNewVersion} className="gap-1.5">
              <Upload className="size-3.5" aria-hidden="true" />
              <span>New version</span>
            </Button>
          ) : null}
        </div>

        {skill.description && <p className="text-xs text-fg-muted mt-1">{skill.description}</p>}
      </div>

      {/* SKILL.md View - Renders as PLAIN TEXT in a <pre> per Plan A10 rules */}
      <section aria-labelledby="skill-instructions-heading" className="flex flex-col gap-2">
        <div className="flex items-center gap-2">
          <FileCode className="size-3.5 text-accent" aria-hidden="true" />
          <h4 id="skill-instructions-heading" className="text-xs font-semibold text-fg">
            SKILL.md Instructions
          </h4>
        </div>

        <pre
          data-slot="skill-markdown"
          className="p-4 rounded-lg bg-surface border border-border text-xs font-mono text-fg whitespace-pre-wrap break-words overflow-x-auto selection:bg-accent/20 max-h-[360px]"
        >
          {skillMarkdown || 'No SKILL.md provided.'}
        </pre>
      </section>

      {/* File List */}
      <section aria-labelledby="skill-files-heading" className="flex flex-col gap-2">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <FileCode className="size-3.5 text-fg-muted" aria-hidden="true" />
            <h4 id="skill-files-heading" className="text-xs font-semibold text-fg">
              Files ({skill.files.length})
            </h4>
          </div>
        </div>

        {skill.files.length === 0 ? (
          <p className="text-2xs text-fg-muted py-2">No files in this skill package.</p>
        ) : (
          <div role="list" aria-label="Skill files" className="flex flex-col gap-1.5">
            {skill.files.map((file) => (
              <div key={file.path} role="listitem" data-slot="skill-file-item">
                {onOpenFile ? (
                  <button
                    type="button"
                    onClick={() => onOpenFile(file.path)}
                    className={cn(
                      'w-full flex items-center justify-between p-2.5 rounded-lg border border-border bg-surface text-xs text-left transition-colors',
                      'hover:bg-surface-raised hover:border-fg-muted/30 cursor-pointer focus-visible:outline-2 focus-visible:outline-accent',
                    )}
                  >
                    <div className="flex items-center gap-2 min-w-0">
                      <FileCode className="size-4 text-fg-muted shrink-0" aria-hidden="true" />
                      <span className="font-mono text-xs text-fg truncate">{file.path}</span>
                    </div>

                    <div className="flex items-center gap-3 shrink-0">
                      <span className="text-2xs text-fg-muted font-sans">
                        {formatBytes(file.size)}
                      </span>
                      <MonoRef className="text-2xs text-fg-muted">
                        {file.sha256.slice(0, 8)}
                      </MonoRef>
                    </div>
                  </button>
                ) : (
                  <div className="flex items-center justify-between p-2.5 rounded-lg border border-border bg-surface text-xs">
                    <div className="flex items-center gap-2 min-w-0">
                      <FileCode className="size-4 text-fg-muted shrink-0" aria-hidden="true" />
                      <span className="font-mono text-xs text-fg truncate">{file.path}</span>
                    </div>

                    <div className="flex items-center gap-3 shrink-0">
                      <span className="text-2xs text-fg-muted font-sans">
                        {formatBytes(file.size)}
                      </span>
                      <MonoRef className="text-2xs text-fg-muted">
                        {file.sha256.slice(0, 8)}
                      </MonoRef>
                    </div>
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </section>

      {/* Version History */}
      <section aria-labelledby="skill-history-heading" className="flex flex-col gap-2">
        <div className="flex items-center gap-2">
          <History className="size-3.5 text-fg-muted" aria-hidden="true" />
          <h4 id="skill-history-heading" className="text-xs font-semibold text-fg">
            Version History
          </h4>
        </div>

        <div role="list" aria-label="Version history" className="flex flex-col gap-1.5">
          {history && history.length > 0 ? (
            history.map((h) => (
              <div
                key={h.hash}
                role="listitem"
                data-slot="skill-history-item"
                className="flex items-center justify-between p-2.5 rounded-lg border border-border bg-surface text-xs"
              >
                <div className="flex items-center gap-2">
                  <span className="font-semibold text-fg">{h.version}</span>
                  <MonoRef className="text-2xs">{h.hash.slice(0, 10)}</MonoRef>
                </div>
                {h.latest ? <span className="text-2xs text-fg-muted">Latest</span> : null}
              </div>
            ))
          ) : (
            <div
              role="listitem"
              data-slot="skill-history-item"
              className="flex items-center justify-between p-2.5 rounded-lg border border-border bg-surface text-xs"
            >
              <div className="flex items-center gap-2">
                <span className="font-semibold text-fg">{provenance}</span>
                <MonoRef className="text-2xs">{skill.hash.slice(0, 10)}</MonoRef>
              </div>
              <span className="text-2xs text-fg-muted">Current</span>
            </div>
          )}
        </div>
      </section>

      {/* Used by */}
      <section aria-labelledby="skill-usages-heading" className="flex flex-col gap-2">
        <div className="flex items-center gap-2">
          <BookOpen className="size-3.5 text-fg-muted" aria-hidden="true" />
          <h4 id="skill-usages-heading" className="text-xs font-semibold text-fg">
            Used by ({skill.usages.length})
          </h4>
        </div>

        {skill.usages.length === 0 ? (
          <p className="text-2xs text-fg-muted py-2">Not used by any runbooks.</p>
        ) : (
          <div role="list" aria-label="Used by runbooks" className="flex flex-col gap-2">
            {skill.usages.map((usage) => {
              const label = describeUsage?.(usage);
              return (
                <div
                  key={`${usage.runbook_id}-${usage.version_id}`}
                  role="listitem"
                  data-slot="skill-usage-item"
                  className="flex flex-col gap-1.5 p-3 rounded-lg border border-border bg-surface text-xs"
                >
                  <div className="flex items-center justify-between flex-wrap gap-2">
                    <div className="flex items-center gap-1.5 min-w-0">
                      <span className="font-sans font-medium text-fg-muted text-2xs uppercase">
                        Runbook:
                      </span>
                      {label ? (
                        <span className="text-xs font-semibold text-fg truncate">
                          {label.runbook}
                        </span>
                      ) : (
                        <MonoRef className="text-xs font-semibold">{usage.runbook_id}</MonoRef>
                      )}
                    </div>
                    <div className="flex items-center gap-1.5">
                      <span className="font-sans font-medium text-fg-muted text-2xs uppercase">
                        Version:
                      </span>
                      {label?.version ? (
                        <span className="text-2xs font-mono text-fg">{label.version}</span>
                      ) : (
                        <MonoRef className="text-2xs">{usage.version_id}</MonoRef>
                      )}
                    </div>
                  </div>

                  <div className="flex items-center gap-1.5 flex-wrap mt-0.5">
                    <span className="text-2xs text-fg-muted">Steps:</span>
                    {usage.step_ids.map((stepId) => (
                      <span
                        key={stepId}
                        className="px-1.5 py-0.5 rounded bg-surface-raised border border-border font-mono text-2xs text-fg"
                      >
                        {stepId}
                      </span>
                    ))}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </section>
    </div>
  );
}
