import { releasedForms } from '@runbook/runner-shared/redact';

export function responseGuard(values: readonly string[]) {
  const patterns = [...new Set(values.flatMap(releasedForms))].map((value) => Buffer.from(value))
    .sort((a, b) => b.length - a.length);
  const replacement = Buffer.from('[REDACTED]');
  return (body: Buffer): Buffer => {
    let bytes = body;
    for (const secret of patterns) {
      const chunks: Buffer[] = [];
      let start = 0;
      for (let match = bytes.indexOf(secret); match !== -1; match = bytes.indexOf(secret, start)) {
        chunks.push(bytes.subarray(start, match), replacement);
        start = match + secret.length;
      }
      if (chunks.length) bytes = Buffer.concat([...chunks, bytes.subarray(start)]);
    }
    return bytes;
  };
}
