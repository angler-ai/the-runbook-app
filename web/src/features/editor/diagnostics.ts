// A diagnostic's text for people (walkthrough #11). runbook-spec raises
// most normalizer diagnostics without a message (`Diagnostic.message` is
// None in crates/runbook-spec/src/normalize.rs), and `wire_diagnostics`
// (crates/runbook-spec/src/authoring.rs) then fills the wire message with
// the code. So "spec.invalid_value spec.invalid_value" reached the page.
// The code stays visible as secondary text; this supplies the sentence.

interface DiagnosticLike {
  code: string;
  message?: string | null;
  path?: string | null;
}

/** `/spec/execution/timeout` → `execution.timeout` (the `spec.` prefix adds nothing). */
function fieldOf(path: string | null | undefined): string {
  const parts = (path ?? '')
    .split('/')
    .slice(1)
    .map((part) => part.replace(/~1/g, '/').replace(/~0/g, '~'));
  if (parts[0] === 'spec' && parts.length > 1) parts.shift();
  return parts.join('.');
}

const KNOWN: Record<string, (field: string) => string> = {
  'spec.invalid_value': (f) => `“${f}” has a value that is not allowed here.`,
  'spec.required': (f) => `“${f}” is required.`,
  'spec.invalid_shape': (f) => `“${f}” does not have the expected shape.`,
  'spec.invalid_schema': (f) => `“${f}” is not a valid schema.`,
  'spec.unknown_field': (f) => `“${f}” is not a field the runbook format knows.`,
  'spec.unsupported_value': (f) => `“${f}” has a value this version does not support.`,
  'spec.unsupported_feature': (f) => `“${f}” uses a feature this version does not support.`,
  'spec.unresolved_user': (f) => `“${f}” names a person the directory does not know.`,
  'spec.unresolved_team': (f) => `“${f}” names a team the directory does not know.`,
  'spec.unresolved_skill': (f) => `“${f}” names a skill the library does not have.`,
  'spec.unknown_variable': (f) => `“${f}” uses a variable that is not declared.`,
  'spec.yaml_syntax': () => 'The YAML does not parse.',
};

/** The human message: the compiler's own, else one made from the code and path. */
export function diagnosticMessage(diagnostic: DiagnosticLike): string {
  const message = diagnostic.message?.trim() ?? '';
  if (message && message !== diagnostic.code) return message;
  const field = fieldOf(diagnostic.path) || 'the runbook';
  const known = KNOWN[diagnostic.code];
  if (known) return known(field);
  return `The compiler reported a problem with “${field}”.`;
}
