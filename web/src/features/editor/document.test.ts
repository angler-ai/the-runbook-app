// The shared YAML Document model (M5): text is the source of truth, the parsed
// view follows it, and JSON pointers resolve to 1-based source positions.
import { describe, expect, it } from 'vitest';
import { pointerSegments, RunbookDocument } from './document';

const SOURCE = `# a comment the model must keep
apiVersion: therunbook.app/v1
metadata:
  id: post-deploy
  title: "Post-deploy"
spec:
  steps:
    - id: smoke
      title: Smoke tests
    - id: a/b~c
`;

describe('RunbookDocument', () => {
  it('reads metadata and follows text replacements', () => {
    const model = new RunbookDocument(SOURCE);
    expect(model.metadata()).toEqual({ id: 'post-deploy', title: 'Post-deploy' });
    model.setText(SOURCE.replace('"Post-deploy"', 'Renamed'));
    expect(model.metadata().title).toBe('Renamed');
    expect(model.text).toContain('# a comment the model must keep');
  });

  it('locates pointers, falling back to the nearest parent', () => {
    const model = new RunbookDocument(SOURCE);
    expect(model.locate('/metadata/id')).toEqual({ line: 4, column: 7 });
    expect(model.locate('/spec/steps/0/title')).toEqual({ line: 9, column: 14 });
    expect(model.locate('/spec/steps/0/nope')).toEqual({ line: 8, column: 7 });
    expect(model.locate('')).toEqual({ line: 2, column: 1 });
  });

  it('decodes RFC 6901 escapes and array indexes', () => {
    expect(pointerSegments('/spec/steps/1/id')).toEqual(['spec', 'steps', 1, 'id']);
    expect(pointerSegments('/a~1b~0c')).toEqual(['a/b~c']);
  });

  it('commits builder mutations back into the text', () => {
    const model = new RunbookDocument(SOURCE);
    model.document.setIn(['metadata', 'title'], 'Built');
    const text = model.commit();
    expect(model.text).toBe(text);
    expect(model.metadata().title).toBe('Built');
    expect(text).toContain('# a comment the model must keep');
  });

  it('reports YAML syntax errors without throwing', () => {
    expect(new RunbookDocument('metadata: [unclosed\n').syntaxErrors.length).toBeGreaterThan(0);
  });
});
