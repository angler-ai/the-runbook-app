import { describe, expect, it } from 'vitest';
import { startedBy, triggerLabel } from './ExecutionTable';

const names = (id: string) => (id === 'usr_1' ? 'Owner' : id);

describe('startedBy', () => {
  it('reads each created_by principal', () => {
    expect(startedBy('human:usr_1')).toEqual({ kind: 'human', user_id: 'usr_1' });
    expect(startedBy('usr_1')).toEqual({ kind: 'human', user_id: 'usr_1' });
    expect(startedBy('agent:usr_1', names)).toMatchObject({ kind: 'agent', name: 'Agent for Owner' });
    expect(startedBy('service:tok_9')).toMatchObject({ kind: 'agent', name: 'Service token' });
    expect(startedBy('system')).toEqual({ kind: 'system' });
    expect(startedBy(null)).toBeNull();
    expect(startedBy('schedule:nightly')).toEqual({ kind: 'system' });
    expect(triggerLabel('schedule:nightly')).toBe('Schedule nightly');
    expect(triggerLabel('webhook:on_deploy')).toBe('Webhook on_deploy');
    expect(triggerLabel('human:usr_1')).toBeUndefined();
  });
});
