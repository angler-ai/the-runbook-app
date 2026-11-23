import type { ReadyItem } from '@runbook/runner-shared/contracts/core';
import { fairOrder } from './fairness.ts';

export class ReadyQueue {
  private readonly entries = new Map<string, ReadyItem>();
  private lastProject?: string;

  add(items: readonly ReadyItem[]): void {
    for (const item of items) this.entries.set(item.step_run_id, item);
  }

  replace(items: readonly ReadyItem[], project?: string): void {
    if (project === undefined) this.entries.clear();
    else {
      for (const [id, entry] of this.entries) if (entry.project_id === project) this.entries.delete(id);
    }
    this.add(items);
  }

  ordered(): ReadyItem[] {
    return fairOrder([...this.entries.values()], this.lastProject);
  }

  admitted(item: ReadyItem): void {
    this.entries.delete(item.step_run_id);
    this.lastProject = item.project_id;
  }

  get size(): number {
    return this.entries.size;
  }
}
