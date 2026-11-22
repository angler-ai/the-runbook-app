import type { ReadyItem } from '@runbook/runner-shared/contracts/core';

export function fairOrder(items: readonly ReadyItem[], afterProject?: string): ReadyItem[] {
  const projects = new Map<string, ReadyItem[]>();
  const sorted = [...items].sort((a, b) => Date.parse(a.ready_at) - Date.parse(b.ready_at) ||
    a.step_run_id.localeCompare(b.step_run_id));
  for (const item of sorted) {
    const list = projects.get(item.project_id) ?? [];
    list.push(item);
    projects.set(item.project_id, list);
  }
  const ids = [...projects.keys()].sort();
  const index = afterProject === undefined ? -1 : ids.findIndex((id) => id > afterProject);
  const ordered = index < 0 ? ids : [...ids.slice(index), ...ids.slice(0, index)];
  const result: ReadyItem[] = [];
  for (let i = 0; result.length < sorted.length; i++) {
    for (const id of ordered) {
      const item = projects.get(id)?.[i];
      if (item) result.push(item);
    }
  }
  return result;
}
