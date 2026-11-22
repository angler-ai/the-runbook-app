import type { components } from '@runbook/runner-shared/core-client';

export const EXCERPT_MAX_BYTES = 8191;

export function clipUtf8(text: string, limit: number): string {
  const bytes = Buffer.from(text, 'utf8');
  if (bytes.length <= limit) return text;
  let end = limit;
  while ((bytes[end]! & 0xc0) === 0x80) end--;
  return bytes.subarray(0, end).toString('utf8');
}

export function transcriptEvidence(coreUrl: string, install: string, step: string, excerpt: string,
  policy: components['schemas']['StepEvidencePolicy'] | null | undefined) {
  const url = `${coreUrl}/v1/runners/${encodeURIComponent(install)}/proxy/runner/v1/transcripts/${encodeURIComponent(step)}`;
  const allowed = (kind: string) => !policy?.accepted || policy.accepted.includes(kind);
  return {
    logs: allowed('log') && excerpt ? [clipUtf8(excerpt, EXCERPT_MAX_BYTES)] : [],
    // The core refuses evidence links that are not https.
    evidence: allowed('link') && url.startsWith('https://') ? [url] : [],
  };
}
