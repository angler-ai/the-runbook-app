import { describe, expect, it } from 'vitest';
import { diagnosticMessage } from './diagnostics';

describe('diagnosticMessage (walkthrough #11)', () => {
  it('keeps the compiler’s own message', () => {
    expect(
      diagnosticMessage({
        code: 'identity.unknown_team',
        message: 'unknown team oncall',
        path: '',
      }),
    ).toBe('unknown team oncall');
  });

  it('says something human when the message only repeats the code', () => {
    // runbook-spec's normalizer raises most diagnostics without a message,
    // and wire_diagnostics then fills the message with the code.
    const text = diagnosticMessage({
      code: 'spec.invalid_value',
      message: 'spec.invalid_value',
      path: '/spec/execution/timeout',
    });
    expect(text).toBe('“execution.timeout” has a value that is not allowed here.');
  });

  it('falls back to a generic sentence for a code it does not know, never the code', () => {
    const text = diagnosticMessage({ code: 'spec.brand_new', message: '', path: '/spec/x' });
    expect(text).not.toContain('spec.brand_new');
    expect(text).toBe('The compiler reported a problem with “x”.');
  });
});
