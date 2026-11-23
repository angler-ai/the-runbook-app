// Structured JSON-line logging for every runner process. Secret values must
// never reach a log (design §7.3), so call sites pass IDs, codes and counts
// as fields, and errors are logged by name and code only (`errorFields`):
// messages from drivers and libraries can quote the values they handled.
// Tests swap the sink to capture every line and grep it for fake secrets.

import { logCorrelation } from './telemetry.ts';
import { loadRedaction } from './redact.ts';
import { findSecretShapedValues } from './secret-shape.ts';

let redact = (text: string) => findSecretShapedValues(text).length ? '[REDACTED]' : text;
const allowedField = /^(?:org_id|project_id|run_id|step_run_id|runner_id|group_id|release_id|credential_id|reservation_id|server_id|component|method|path|status|provider_status|ms|code|error|error_class|provider|model|field|version|expires_at|attempt|delay_ms|reason|ok|host|port|max|acquire_timeout_ms|count|fingerprint)$/;

export async function initLogRedaction() {
  const core = await loadRedaction();
  const tokens = Object.entries(process.env).flatMap(([key, value]) => {
    if (!value) return [];
    const notifiers = key === 'RUNNER_WORKER_NOTIFIERS';
    const secrets = /(?:TOKEN|SECRET|PASSWORD|DATABASE_URL)/.test(key) ? [value] : [];
    if (notifiers || /^RUNNER_WORKER_(?:API|VAULT|GATEWAY)_TOKENS$/.test(key)) {
      try {
        const map: unknown = JSON.parse(value);
        if (map && typeof map === 'object' && !Array.isArray(map)) {
          for (const entry of Object.values(map) as unknown[]) {
            const token = notifiers && entry && typeof entry === 'object' && 'token' in entry ? entry.token : entry;
            if (typeof token === 'string') secrets.push(token);
          }
        }
      } catch {
        // Configuration validation reports malformed maps without their contents.
      }
    }
    return secrets;
  }).filter((value) => value.length >= 8);
  redact = (text) => {
    const clean = core.redact(text, tokens);
    return findSecretShapedValues(clean).length ? '[REDACTED]' : clean;
  };
}

const severity = { debug: 'DEBUG', info: 'INFO', warn: 'WARNING', error: 'ERROR' };
const warningWindows = new Map<string, { count: number; flush: () => void }>();
const warningIntervalMs = 60_000;

export type LogLevel = 'debug' | 'info' | 'warn' | 'error';
export type LogFields = Record<string, unknown>;
export type LogSink = (line: string) => void;

let sink: LogSink = (line) => process.stdout.write(line + '\n');

/** Replace the process-wide sink; returns the previous one. */
export function setLogSink(next: LogSink): LogSink {
  const previous = sink;
  sink = next;
  return previous;
}

export interface Logger {
  debug(msg: string, fields?: LogFields): void;
  info(msg: string, fields?: LogFields): void;
  warn(msg: string, fields?: LogFields): void;
  error(msg: string, fields?: LogFields): void;
  child(fields: LogFields): Logger;
}

export function createLogger(role: string, base: LogFields = {}): Logger {
  const write = (level: LogLevel, msg: string, fields: LogFields = {}) => {
    if (level === 'debug' && process.env.RUNBOOK_LOG_LEVEL !== 'debug') return;
    const safeFields = Object.fromEntries(Object.entries({ ...base, ...fields })
      .filter(([key, value]) => allowedField.test(key) && (value === null || ['string', 'number', 'boolean'].includes(typeof value)))
      .map(([key, value]) => [key, typeof value === 'string' ? redact(value) : value]));
    const message = redact(msg);
    const record = {
      ...safeFields, severity: severity[level], message,
      component: safeFields.component ?? role, level, process: role, msg: message,
      ...logCorrelation(safeFields, role),
    };
    if (level === 'warn' || level === 'error') {
      const key = JSON.stringify([role, level, message, safeFields.code, safeFields.reason, safeFields.error_class,
        safeFields.method, safeFields.path, safeFields.status, safeFields.step_run_id, safeFields.credential_id,
        safeFields.release_id, safeFields.runner_id]);
      const existing = warningWindows.get(key);
      if (existing) {
        existing.count++;
        return;
      }
      if (warningWindows.size >= 512) warningWindows.values().next().value?.flush();
      const window = { count: 0, flush };
      const timer = setTimeout(flush, warningIntervalMs).unref();
      warningWindows.set(key, window);
      function flush() {
        clearTimeout(timer);
        warningWindows.delete(key);
        if (window.count) sink(JSON.stringify({ ...record, time: new Date().toISOString(), suppressed: window.count }));
      }
    }
    sink(JSON.stringify({ ...record, time: new Date().toISOString() }));
  };
  return {
    debug: (m, f) => write('debug', m, f),
    info: (m, f) => write('info', m, f),
    warn: (m, f) => write('warn', m, f),
    error: (m, f) => write('error', m, f),
    child: (f) => createLogger(role, { ...base, ...f }),
  };
}

/** What a log line may say about an error: its class and code, never its message. */
export function errorFields(error: unknown): LogFields {
  if (error instanceof Error) {
    const code = (error as { code?: unknown }).code;
    return { error: error.name, ...(typeof code === 'string' && /^[A-Za-z0-9_.-]{1,64}$/.test(code) ? { code } : {}) };
  }
  return { error: 'non-error' };
}

/** Capture log lines in a test. */
export function captureLogs(): { lines: string[]; text(): string; restore(): void } {
  const lines: string[] = [];
  const previous = setLogSink((line) => lines.push(line));
  return { lines, text: () => lines.join('\n'), restore: () => setLogSink(previous) };
}
