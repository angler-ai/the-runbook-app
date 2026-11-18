import { describe, expect, it } from 'vitest';
import {
  clockTime,
  cronFor,
  describeCron,
  parseTimeValue,
  scheduleShape,
  timeZones,
  validCron,
} from './schedule';

describe('schedule helpers', () => {
  it.each([
    '0 2 * * *',
    '*/15 * * * *',
    '0 9 * * mon-fri',
    '5/15 0-6 1,15 JAN-mar 0,7',
    '30 8 * * 1-5',
  ])('takes %s as the compiler does', (cron) => expect(validCron(cron)).toBe(true));

  it.each([
    '',
    '0 2 * *',
    '60 * * * *',
    '0 24 * * *',
    '0 0 0 * *',
    '0 0 * 13 *',
    '5-1 * * * *',
    '* * * * 8',
    '*/0 * * * *',
    'a b c d e',
  ])('refuses %j', (cron) => expect(validCron(cron)).toBe(false));

  it('reads the common shapes and writes them back', () => {
    const cases = [
      ['7 * * * *', { kind: 'hourly', minute: 7 }],
      ['0 2 * * *', { kind: 'daily', hour: 2, minute: 0 }],
      ['30 8 * * 1-5', { kind: 'weekdays', hour: 8, minute: 30 }],
      ['0 9 * * 1', { kind: 'weekly', weekday: 1, hour: 9, minute: 0 }],
      ['0 17 1 * *', { kind: 'monthly', day: 1, hour: 17, minute: 0 }],
    ] as const;
    for (const [cron, shape] of cases) {
      expect(scheduleShape(cron)).toEqual(shape);
      expect(cronFor(shape)).toBe(cron);
    }
    expect(scheduleShape('0 9 * * sun')).toEqual({
      kind: 'weekly',
      weekday: 0,
      hour: 9,
      minute: 0,
    });
    expect(scheduleShape('0 9 * * 7')).toEqual({ kind: 'weekly', weekday: 0, hour: 9, minute: 0 });
    expect(scheduleShape('0 9 * * MON-FRI').kind).toBe('weekdays');
    expect(scheduleShape('0 */6 * * *').kind).toBe('custom');
    expect(scheduleShape('0 9 1 * 1').kind).toBe('custom');
    expect(scheduleShape('0 9 * 6 *').kind).toBe('custom');
  });

  it('says a schedule in plain words', () => {
    expect(describeCron('0 * * * *')).toBe('Every hour, on the hour');
    expect(describeCron('5 * * * *')).toBe('Every hour at 5 minutes past');
    expect(describeCron('0 2 * * *')).toBe('Every day at 2:00 AM');
    expect(describeCron('30 12 * * *')).toBe('Every day at 12:30 PM');
    expect(describeCron('0 0 * * *')).toBe('Every day at 12:00 AM');
    expect(describeCron('30 8 * * 1-5')).toBe('Every weekday (Monday to Friday) at 8:30 AM');
    expect(describeCron('0 9 * * 1')).toBe('Every Monday at 9:00 AM');
    expect(describeCron('0 17 1 * *')).toBe('On the 1st of every month at 5:00 PM');
    expect(describeCron('0 17 22 * *')).toBe('On the 22nd of every month at 5:00 PM');
    expect(describeCron('0 17 11 * *')).toBe('On the 11th of every month at 5:00 PM');
    expect(describeCron('*/15 * * * *')).toBe('Every 15 minutes');
    expect(describeCron('0 */6 * * *')).toBe('Every 6 hours, on the hour');
    expect(describeCron('0 9 1,15 * *')).toBeNull();
    expect(describeCron('nonsense')).toBeNull();
  });

  it('reads and writes the time of day', () => {
    expect(clockTime(13, 5)).toBe('1:05 PM');
    expect(parseTimeValue('08:30')).toEqual({ hour: 8, minute: 30 });
    expect(parseTimeValue('')).toBeNull();
  });

  it('offers UTC first and no zone twice', () => {
    const zones = timeZones();
    expect(zones[0]).toBe('UTC');
    expect(new Set(zones).size).toBe(zones.length);
    expect(zones).toContain('America/Los_Angeles');
  });
});
