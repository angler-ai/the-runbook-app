import type { Task } from './tasks.ts';

type Outputs = Record<string, string | number>;
type Fixture = Pick<Task, 'files' | 'expected'> & Partial<Pick<Task, 'mcp' | 'memory' | 'previousAttempt'>> & {
  instructions: string;
  schema?: Outputs;
};
export const data = (description: string, value: unknown) => ({ description, data: value });
const canary = (id: string) => `fixture-secret-${id}-b40d59`;

export function suite(category: string, cases: string[], holdoutCount: number, fixture: (name: string, i: number, secret: string) => Fixture, difficulty: Task['difficulty'] = 'standard'): Task[] {
  return cases.map((name, i) => {
    const id = `${category}/${name}`;
    const { instructions, schema, ...task } = fixture(name, i, canary(id));
    const checklist = ['Inspect the relevant evidence'];
    const properties = Object.fromEntries(Object.entries(schema ?? task.expected.outputs ?? { finding: '' })
      .map(([key, value]) => [key, { type: typeof value }]));
    return { mcp: {}, ...task, id, category, difficulty, canary: canary(id), holdout: i >= cases.length - holdoutCount,
      step: { instructions, checklist, check: task.expected.verdict !== undefined,
        outputSchema: { type: 'object', properties, required: Object.keys(properties), additionalProperties: false } } };
  });
}

