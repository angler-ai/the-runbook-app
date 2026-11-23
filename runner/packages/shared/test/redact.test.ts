import { readFileSync } from 'node:fs';
import { beforeAll, describe, expect, it } from 'vitest';
import { loadRedaction, type Redaction, releasedForms } from '../src/redact.ts';

// The cases of crates/runbook-core/tests/redact.rs, checked natively against
// the same redactor by crates/runbook-redact-wasm/tests/fixtures.rs.
const fixtures = JSON.parse(
  readFileSync(new URL('../../../../crates/runbook-redact-wasm/tests/redact_fixtures.json', import.meta.url), 'utf8'),
) as { cases: { input: string; expected: string }[] };

const RELEASED = 'fake-released-Value_0123456789';

describe('redaction', () => {
  let redaction: Redaction;
  beforeAll(async () => {
    redaction = await loadRedaction();
  });

  it('redacts_known_token_shapes_like_core', () => {
    expect(fixtures.cases.length).toBeGreaterThanOrEqual(30);
    for (const { input, expected } of fixtures.cases) {
      expect(redaction.redact(input)).toBe(expected);
      // Every split point, as the core's test does.
      for (let split = 0; split <= input.length; split++) {
        const stream = redaction.stream();
        const out = stream.push(input.slice(0, split)) + stream.push(input.slice(split)) + stream.finish();
        stream.dispose();
        expect(out, `split=${split} input=${JSON.stringify(input)}`).toBe(expected);
      }
    }
  });

  it('redacts_exact_released_value_and_encodings', () => {
    const raw = Buffer.from(RELEASED);
    const text = [
      `plain ${RELEASED} end`,
      `b64 ${raw.toString('base64')} end`,
      `b64url ${raw.toString('base64url')} end`,
      `url ${encodeURIComponent(RELEASED + '/&?')} end`,
      `json ${JSON.stringify({ v: RELEASED })} end`,
      // Inside a longer base64 value at each byte alignment (Basic auth).
      `basic ${Buffer.from(`user:${RELEASED}`).toString('base64')} end`,
      `basic2 ${Buffer.from(`us:${RELEASED}!`).toString('base64')} end`,
      `basic3 ${Buffer.from(`u:${RELEASED}!!`).toString('base64')} end`,
      `hex ${raw.toString('hex')} end`,
    ].join('\n');
    const out = redaction.redact(text, [RELEASED]);
    for (const form of releasedForms(RELEASED)) expect(out).not.toContain(form);
    expect(out).not.toContain(RELEASED);
    expect(out).not.toContain(raw.toString('base64').slice(4, 24));
    expect(out).not.toContain(raw.toString('hex'));
    expect(out).toContain('[REDACTED:released]');
    expect(out).toContain('plain [REDACTED:released] end');
    // Text around the value survives.
    expect(out).toMatch(/^plain .* end$/m);
  });

  it('streaming_split_secret_is_redacted', () => {
    const input = `before ${RELEASED} after`;
    for (let split = 0; split <= input.length; split++) {
      const stream = redaction.stream([RELEASED]);
      const out = stream.push(input.slice(0, split)) + stream.push(input.slice(split)) + stream.finish();
      stream.dispose();
      expect(out, `split=${split}`).toBe('before [REDACTED:released] after');
    }
    // One character at a time, with a core token shape mixed in.
    const mixed = `x ${RELEASED} password=hunter2 ${RELEASED}`;
    const stream = redaction.stream([RELEASED]);
    let out = '';
    for (const ch of mixed) out += stream.push(ch);
    out += stream.finish();
    stream.dispose();
    expect(out).toBe('x [REDACTED:released] password=[REDACTED:password] [REDACTED:released]');
  });

  it('keeps short released values to exact matches and leaves plain text alone', () => {
    expect(redaction.redact('the cat sat', ['cat'])).toBe('the [REDACTED:released] sat');
    expect(redaction.redact('nothing to see here', [RELEASED])).toBe('nothing to see here');
    expect(redaction.redact('emoji 🎉 split', [])).toBe('emoji 🎉 split');
  });

  it('a surrogate pair split across pushes survives', () => {
    const stream = redaction.stream();
    const s = '🎉';
    const out = stream.push('a' + s[0]) + stream.push(s[1] + 'b') + stream.finish();
    stream.dispose();
    expect(out).toBe('a🎉b');
  });
});
