import type { components } from '../../api/schema';
import { EmptyState } from '../layout/EmptyState';
import { ListPaneItem, ListPaneItems } from '../layout/ListPane';
import { Sparkles, BookOpen } from 'lucide-react';

export type SkillVersionView = components['schemas']['SkillVersionView'];

export interface SkillListProps {
  skills: SkillVersionView[];
  selectedName?: string;
  onSelect?: (skill: SkillVersionView) => void;
  className?: string;
}

export function SkillList({ skills, selectedName, onSelect, className }: SkillListProps) {
  if (skills.length === 0) {
    return (
      <EmptyState
        icon={Sparkles}
        title="No skills found"
        description="No skills have been registered in this project yet."
        className={className}
      />
    );
  }

  return (
    <div data-slot="skill-list" className={className}>
      <ListPaneItems label="Skills">
        {skills.map((skill) => {
          // Distinct runbooks using this skill.
          const runbookCount = new Set(skill.usages.map((u) => u.runbook_id)).size;
          return (
            <ListPaneItem
              key={skill.name}
              slot="skill-list-item"
              selected={selectedName === skill.name}
              onSelect={() => onSelect?.(skill)}
              icon={<Sparkles className="size-4" aria-hidden="true" />}
              title={skill.name}
              trailing={
                <span
                  data-slot="skill-usage-count"
                  className="inline-flex items-center gap-1 text-2xs text-fg-muted"
                >
                  <BookOpen className="size-3 shrink-0" aria-hidden="true" />
                  {runbookCount} {runbookCount === 1 ? 'runbook' : 'runbooks'}
                </span>
              }
              detail={skill.description || 'No description provided.'}
            />
          );
        })}
      </ListPaneItems>
    </div>
  );
}
