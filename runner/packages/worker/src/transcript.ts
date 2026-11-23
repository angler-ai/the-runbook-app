import { MAX_TRANSCRIPT_BYTES, type TranscriptRole } from '@runbook/runner-shared/contracts/internal';
import { redactValue, type Redaction } from '@runbook/runner-shared/redact';
import { StepStopped } from './loop/limits.ts';
import { clipUtf8, EXCERPT_MAX_BYTES } from './evidence.ts';

export function transcriptWriter(append: (role: TranscriptRole, value: unknown) => Promise<void>, redaction: Redaction,
  exactRedact: (value: unknown, role: TranscriptRole) => Promise<unknown>, signal?: AbortSignal) {
  let excerpt = '';
  async function sanitize(input: unknown, role: TranscriptRole) {
    signal?.throwIfAborted();
    const exact = await exactRedact(input, role);
    signal?.throwIfAborted();
    return redactValue(exact, redaction.redact);
  }
  return {
    sanitize,
    async record(role: TranscriptRole, value: unknown, options?: { sanitized?: boolean; clipPrompt?: boolean }) {
      const prompt = role === 'system' || role === 'input';
      const tooLarge = () => new StepStopped(role === 'system' ? 'redaction_failed' : 'input_too_large');
      if (prompt && !options?.clipPrompt && Buffer.byteLength(JSON.stringify({ role, value })) > MAX_TRANSCRIPT_BYTES) throw tooLarge();
      let safe = options?.sanitized ? redactValue(value, redaction.redact) : await sanitize(value, role);
      let recorded = safe;
      if (Buffer.byteLength(JSON.stringify({ role, value: safe })) > MAX_TRANSCRIPT_BYTES) {
        if (prompt && !options?.clipPrompt) throw tooLarge();
        const clipped = clipUtf8(JSON.stringify(safe), MAX_TRANSCRIPT_BYTES / 2 - 64) + '\n[TRUNCATED]';
        if (role === 'tool') safe = clipped;
        recorded = clipped;
      }
      signal?.throwIfAborted();
      await append(role, recorded);
      if (role === 'output') excerpt = clipUtf8(JSON.stringify(safe), EXCERPT_MAX_BYTES);
      return safe;
    },
    excerpt: () => excerpt,
  };
}
