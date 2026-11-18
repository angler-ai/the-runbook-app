// Support for reading sign-in codes from the capture transport's JSONL file (D48).
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

/**
 * A message recorded by the auth service's test capture transport (D48).
 * Appended as one JSON line per code to RUNBOOK_AUTH_CAPTURE_FILE.
 * Format: {"at": ISO-8601, "to": email, "type": "sign-in", "code": "123456", "subject": string}
 */
export interface CapturedMail {
  at: string;
  to: string;
  type: string;
  code: string;
  subject: string;
}

/**
 * Returns the path to the capture file from the environment or fallback.
 */
export function captureFilePath(override?: string): string {
  if (override) return override;
  if (process.env.RUNBOOK_AUTH_CAPTURE_FILE) {
    return process.env.RUNBOOK_AUTH_CAPTURE_FILE;
  }
  const pilotDir = process.env.PILOT_DIR;
  if (pilotDir) {
    return join(pilotDir, 'mail.jsonl');
  }
  return join('/tmp', 'runbook-auth-capture.jsonl');
}

/**
 * Reads and parses all captured messages from the capture file.
 * Returns an empty array if the file does not exist or contains no valid lines.
 */
export function readCapturedMail(filePath?: string): CapturedMail[] {
  const path = captureFilePath(filePath);
  if (!existsSync(path)) return [];

  const content = readFileSync(path, 'utf8');
  const messages: CapturedMail[] = [];

  for (const line of content.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    try {
      const parsed = JSON.parse(trimmed) as unknown;
      if (
        parsed &&
        typeof parsed === 'object' &&
        'code' in parsed &&
        typeof (parsed as CapturedMail).code === 'string' &&
        'to' in parsed &&
        typeof (parsed as CapturedMail).to === 'string'
      ) {
        messages.push(parsed as CapturedMail);
      }
    } catch {
      // Ignore partial or malformed lines
    }
  }

  return messages;
}

/**
 * Clears or truncates the captured mail file, typically called before a test.
 */
export function clearCapturedMail(filePath?: string): void {
  const path = captureFilePath(filePath);
  if (existsSync(path)) {
    writeFileSync(path, '', 'utf8');
  }
}

/**
 * Returns the most recent captured mail sent to `to`.
 */
export function latestMailFor(to: string, filePath?: string): CapturedMail | undefined {
  const all = readCapturedMail(filePath);
  const normalizedTo = to.trim().toLowerCase();
  for (let i = all.length - 1; i >= 0; i--) {
    const item = all[i];
    if (item && item.to.trim().toLowerCase() === normalizedTo) {
      return item;
    }
  }
  return undefined;
}

/**
 * Returns the latest OTP code sent to `to`.
 */
export function latestCodeFor(to: string, filePath?: string): string | undefined {
  return latestMailFor(to, filePath)?.code;
}

/**
 * Polls the capture file until a code is captured for `to`, or rejects on timeout.
 */
export async function waitForCode(
  to: string,
  options: {
    filePath?: string;
    timeoutMs?: number;
    intervalMs?: number;
    since?: string;
  } = {},
): Promise<string> {
  const { filePath, timeoutMs = 5000, intervalMs = 100, since } = options;
  const deadline = Date.now() + timeoutMs;
  const normalizedTo = to.trim().toLowerCase();
  const sinceTime = since ? new Date(since).getTime() : 0;

  while (Date.now() < deadline) {
    const all = readCapturedMail(filePath);
    for (let i = all.length - 1; i >= 0; i--) {
      const item = all[i];
      if (!item) continue;
      if (item.to.trim().toLowerCase() === normalizedTo) {
        if (!sinceTime || new Date(item.at).getTime() >= sinceTime) {
          return item.code;
        }
      }
    }
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }

  throw new Error(
    `Timed out after ${timeoutMs}ms waiting for sign-in code sent to ${to} in ${captureFilePath(filePath)}`,
  );
}
