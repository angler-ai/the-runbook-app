import { act, renderHook } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Schemas } from '../../api/client';
import { fixtures } from '../../test/msw/fixtures';
import { describeChanges, useStatusAnnouncements } from './announce';

type Doc = Schemas['ExecutionDocumentView'];

function doc(statuses: Schemas['StepStatus'][], run: Schemas['ExecutionStatus'] = 'running'): Doc {
  const base = structuredClone(fixtures.executionDocument) as Doc;
  base.header.status = run;
  base.sections = statuses.map((status, i) => ({
    ...(base.sections[0] as Doc['sections'][number]),
    step_run_id: `sr_${i}`,
    title: `Step title ${i}`,
    status,
  }));
  return base;
}

describe('describeChanges (U11)', () => {
  it('says nothing for the first document', () => {
    expect(describeChanges(undefined, doc(['ready']))).toEqual([]);
  });

  it('names each step whose status changed, and the run', () => {
    expect(
      describeChanges(doc(['claimed', 'pending']), doc(['succeeded', 'ready'], 'succeeded')),
    ).toEqual(['Step 1, Step title 0: succeeded', 'Step 2, Step title 1: ready', 'Run succeeded']);
  });

  it('ignores unchanged steps and new pending ones', () => {
    expect(describeChanges(doc(['ready']), doc(['ready', 'pending']))).toEqual([]);
    expect(describeChanges(doc(['ready']), doc(['ready', 'timed_out']))).toEqual([
      'Step 2, Step title 1: timed out',
    ]);
  });
});

describe('useStatusAnnouncements (U11)', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('batches a burst of changes into one announcement per window', () => {
    vi.useFakeTimers();
    const { result, rerender } = renderHook(({ d }) => useStatusAnnouncements(d, 1000), {
      initialProps: { d: doc(['ready', 'pending', 'pending']) },
    });
    rerender({ d: doc(['claimed', 'pending', 'pending']) });
    rerender({ d: doc(['succeeded', 'ready', 'pending']) });
    act(() => {
      vi.advanceTimersByTime(0);
    });
    // A burst between renders is one announcement.
    expect(result.current.map((a) => a.text)).toEqual([
      'Step 1, Step title 0: claimed. Step 1, Step title 0: succeeded. Step 2, Step title 1: ready.',
    ]);
    // The next change waits for the window to pass.
    rerender({ d: doc(['succeeded', 'claimed', 'ready', 'ready', 'ready'], 'waiting') });
    act(() => {
      vi.advanceTimersByTime(999);
    });
    expect(result.current).toHaveLength(1);
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(result.current.at(-1)?.text).toBe(
      'Step 2, Step title 1: claimed. Step 3, Step title 2: ready. Step 4, Step title 3: ready. And 2 more changes.',
    );
  });
});
