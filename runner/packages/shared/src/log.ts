// Structured JSON-line logging for every runner process. Secret values must
// never reach a log (design §7.3), so call sites pass IDs, codes and counts
// as fields, and errors are logged by name and code only (`errorFields`):
// messages from drivers and libraries can quote the values they handled.
// Tests swap the sink to capture every line and grep it for fake secrets.

export type LogLevel = 'debug' | 'info' | 'warn' | 'error';
export type LogFields = Record<string, unknown>;
export type LogSink = (line: string) => void;

let sink: LogSink = (line) => process.stderr.write(line + '\n');

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

export function createLogger(process: string, base: LogFields = {}): Logger {
  const write = (level: LogLevel, msg: string, fields: LogFields = {}) => {
    sink(JSON.stringify({ time: new Date().toISOString(), level, process, msg, ...base, ...fields }));
  };
  return {
    debug: (m, f) => write('debug', m, f),
    info: (m, f) => write('info', m, f),
    warn: (m, f) => write('warn', m, f),
    error: (m, f) => write('error', m, f),
    child: (f) => createLogger(process, { ...base, ...f }),
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
