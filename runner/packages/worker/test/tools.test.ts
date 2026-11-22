import { execFileSync } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { expect, it, vi } from 'vitest';
import { workspaceTools } from '../src/loop/tools.ts';

const signal = new AbortController().signal;
const ctx = { capability: () => 'grant', signal } as Parameters<typeof workspaceTools>[1];

it('grep finds individual lines with their line numbers', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'runbook-grep-'));
  try {
    await writeFile(join(directory, 'log.txt'), 'first\nfound this\nskip\nfound also\n');
    const clients = { request: async (_process: string, _route: string, { body }: { body: { argv: string[] } }) => {
      const [, , script, ...args] = body.argv;
      return { stdout: execFileSync(process.execPath,
        ['-e', script!.replace("'/workspace'", JSON.stringify(directory)), ...args], { encoding: 'utf8' }) };
    } } as unknown as Parameters<typeof workspaceTools>[0];
    const grep = workspaceTools(clients, ctx, 'sandbox', 3000).find(({ name }) => name === 'grep')!;
    await expect(grep.call({ pattern: 'found' }, signal)).resolves.toEqual({
      stdout: 'log.txt:2:found this\nlog.txt:4:found also',
    });
  } finally {
    await rm(directory, { recursive: true });
  }
});

it('edit refuses to write when the read content was redacted', async () => {
  const request = vi.fn(async () => ({ content: 'before [REDACTED] after', redacted: true }));
  const edit = workspaceTools({ request } as unknown as Parameters<typeof workspaceTools>[0], ctx,
    'sandbox', 3000).find(({ name }) => name === 'edit')!;
  await expect(edit.call({ path: 'note', old_text: 'after', new_text: 'changed' }, signal)).rejects.toThrow('redacted');
  expect(request).toHaveBeenCalledOnce();
});
