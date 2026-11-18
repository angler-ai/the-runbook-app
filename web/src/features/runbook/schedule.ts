// Schedule triggers in plain words, and the common shapes the builder's
// form offers (every hour, every day, every weekday, every week, every
// month). The cron rules mirror runbook-spec's `cron.rs`: five fields,
// `*`, lists, ranges, steps, month and weekday names, Sunday as 0 or 7.
// The compiler stays the judge; this only reads and writes.

const MONTHS = ['jan', 'feb', 'mar', 'apr', 'may', 'jun', 'jul', 'aug', 'sep', 'oct', 'nov', 'dec'];
const WEEKDAYS = ['sun', 'mon', 'tue', 'wed', 'thu', 'fri', 'sat'];
export const WEEKDAY_NAMES = [
  'Sunday',
  'Monday',
  'Tuesday',
  'Wednesday',
  'Thursday',
  'Friday',
  'Saturday',
] as const;

function value(text: string, min: number, max: number, names: readonly string[]): number | null {
  const named = names.findIndex((n) => n === text.toLowerCase());
  if (named >= 0) return min + named;
  if (!/^\d{1,2}$/.test(text)) return null;
  const n = Number(text);
  return n >= min && n <= max ? n : null;
}

function validField(text: string, min: number, max: number, names: readonly string[]): boolean {
  if (text === '') return false;
  return text.split(',').every((part) => {
    const [range = '', step, extra] = part.split('/');
    if (extra !== undefined) return false;
    if (step !== undefined && value(step, 1, Math.max(max, 1), []) === null) return false;
    if (range === '*') return true;
    const bounds = range.split('-');
    if (bounds.length === 2) {
      const a = value(bounds[0] ?? '', min, max, names);
      const b = value(bounds[1] ?? '', min, max, names);
      return a !== null && b !== null && a <= b;
    }
    return bounds.length === 1 && value(range, min, max, names) !== null;
  });
}

/** Whether the compiler would take this cron expression. */
export function validCron(cron: string): boolean {
  const parts = cron.trim().split(/\s+/);
  if (parts.length !== 5) return false;
  const [minute, hour, day, month, weekday] = parts as [string, string, string, string, string];
  return (
    validField(minute, 0, 59, []) &&
    validField(hour, 0, 23, []) &&
    validField(day, 1, 31, []) &&
    validField(month, 1, 12, MONTHS) &&
    validField(weekday, 0, 7, WEEKDAYS)
  );
}

export type ScheduleShape =
  | { kind: 'hourly'; minute: number }
  | { kind: 'daily'; hour: number; minute: number }
  | { kind: 'weekdays'; hour: number; minute: number }
  | { kind: 'weekly'; weekday: number; hour: number; minute: number }
  | { kind: 'monthly'; day: number; hour: number; minute: number }
  | { kind: 'custom' };

export type ScheduleKind = ScheduleShape['kind'];

function single(text: string, min: number, max: number, names: readonly string[] = []) {
  return value(text, min, max, names);
}

/** The form's shape for a cron expression, or `custom` when it is something else. */
export function scheduleShape(cron: string): ScheduleShape {
  if (!validCron(cron)) return { kind: 'custom' };
  const [m = '', h = '', day = '', month = '', weekday = ''] = cron.trim().split(/\s+/);
  const minute = single(m, 0, 59);
  if (minute === null || month !== '*') return { kind: 'custom' };
  if (h === '*' && day === '*' && weekday === '*') return { kind: 'hourly', minute };
  const hour = single(h, 0, 23);
  if (hour === null) return { kind: 'custom' };
  if (day === '*' && weekday === '*') return { kind: 'daily', hour, minute };
  if (day === '*' && /^(1|mon)-(5|fri)$/i.test(weekday)) return { kind: 'weekdays', hour, minute };
  if (day === '*') {
    const w = single(weekday, 0, 7, WEEKDAYS);
    return w === null ? { kind: 'custom' } : { kind: 'weekly', weekday: w % 7, hour, minute };
  }
  if (weekday === '*') {
    const d = single(day, 1, 31);
    return d === null ? { kind: 'custom' } : { kind: 'monthly', day: d, hour, minute };
  }
  return { kind: 'custom' };
}

/** The cron expression for a shape (a custom one has none of its own). */
export function cronFor(shape: Exclude<ScheduleShape, { kind: 'custom' }>): string {
  switch (shape.kind) {
    case 'hourly':
      return `${shape.minute} * * * *`;
    case 'daily':
      return `${shape.minute} ${shape.hour} * * *`;
    case 'weekdays':
      return `${shape.minute} ${shape.hour} * * 1-5`;
    case 'weekly':
      return `${shape.minute} ${shape.hour} * * ${shape.weekday}`;
    case 'monthly':
      return `${shape.minute} ${shape.hour} ${shape.day} * *`;
  }
}

/** The time of day a shape runs at, for switching between shapes. */
export function shapeTime(shape: ScheduleShape): { hour: number; minute: number } {
  if (shape.kind === 'custom') return { hour: 9, minute: 0 };
  if (shape.kind === 'hourly') return { hour: 9, minute: shape.minute };
  return { hour: shape.hour, minute: shape.minute };
}

/** `9:05 AM`, `12:00 PM`. */
export function clockTime(hour: number, minute: number): string {
  const h12 = hour % 12 === 0 ? 12 : hour % 12;
  return `${h12}:${String(minute).padStart(2, '0')} ${hour < 12 ? 'AM' : 'PM'}`;
}

/** `HH:MM`, the value an `<input type="time">` holds. */
export function timeValue(hour: number, minute: number): string {
  return `${String(hour).padStart(2, '0')}:${String(minute).padStart(2, '0')}`;
}

/** The hour and minute of an `<input type="time">` value, or null. */
export function parseTimeValue(text: string): { hour: number; minute: number } | null {
  const match = /^(\d{2}):(\d{2})/.exec(text);
  if (!match) return null;
  const hour = Number(match[1]);
  const minute = Number(match[2]);
  return hour <= 23 && minute <= 59 ? { hour, minute } : null;
}

function ordinal(n: number): string {
  const tens = n % 100;
  if (tens >= 11 && tens <= 13) return `${n}th`;
  return `${n}${['th', 'st', 'nd', 'rd'][n % 10] ?? 'th'}`;
}

/**
 * A schedule in plain words: "Every weekday at 9:00 AM". Expressions the
 * form doesn't offer get a short reading when they are a simple interval,
 * else null (the caller shows the cron itself).
 */
export function describeCron(cron: string): string | null {
  if (!validCron(cron)) return null;
  const shape = scheduleShape(cron);
  switch (shape.kind) {
    case 'hourly':
      return shape.minute === 0
        ? 'Every hour, on the hour'
        : `Every hour at ${shape.minute} minute${shape.minute === 1 ? '' : 's'} past`;
    case 'daily':
      return `Every day at ${clockTime(shape.hour, shape.minute)}`;
    case 'weekdays':
      return `Every weekday (Monday to Friday) at ${clockTime(shape.hour, shape.minute)}`;
    case 'weekly':
      return `Every ${WEEKDAY_NAMES[shape.weekday]} at ${clockTime(shape.hour, shape.minute)}`;
    case 'monthly':
      return `On the ${ordinal(shape.day)} of every month at ${clockTime(shape.hour, shape.minute)}`;
    case 'custom':
      break;
  }
  const [m, h, day, month, weekday] = cron.trim().split(/\s+/);
  const rest = day === '*' && month === '*' && weekday === '*';
  const every = /^\*\/(\d+)$/;
  if (rest && h === '*' && every.test(m ?? '')) {
    const n = Number(every.exec(m ?? '')?.[1]);
    return n === 1 ? 'Every minute' : `Every ${n} minutes`;
  }
  if (rest && m === '0' && every.test(h ?? '')) {
    const n = Number(every.exec(h ?? '')?.[1]);
    return n === 1 ? 'Every hour, on the hour' : `Every ${n} hours, on the hour`;
  }
  return null;
}

const COMMON_ZONES = [
  'UTC',
  'America/Los_Angeles',
  'America/Denver',
  'America/Chicago',
  'America/New_York',
  'Europe/London',
  'Europe/Berlin',
  'Asia/Kolkata',
  'Asia/Singapore',
  'Asia/Tokyo',
  'Australia/Sydney',
];

/** The IANA time zones to offer, with UTC first. */
export function timeZones(): string[] {
  const intl = Intl as typeof Intl & { supportedValuesOf?: (key: string) => string[] };
  let zones: string[] = [];
  try {
    zones = intl.supportedValuesOf?.('timeZone') ?? [];
  } catch {
    zones = [];
  }
  if (zones.length === 0) zones = COMMON_ZONES;
  return ['UTC', ...zones.filter((z) => z !== 'UTC')];
}

/** This browser's time zone, the one a new schedule starts with. */
export function browserTimeZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
  } catch {
    return 'UTC';
  }
}
