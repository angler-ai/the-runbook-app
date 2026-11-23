import { expect, it, vi } from 'vitest';
import type { ExecutionContext } from '../src/claim.ts';
import type { workerClients } from '../src/clients.ts';
import { workspaceToolsV2 } from '../src/loop/workspace-v2.ts';

it('uses the requested Bash deadline even when it exceeds the working-tool default', async () => {
  const request = vi.fn(async () => ({ exit_code: 0, stdout: 'done', stderr: '' }));
  const ctx = { capability: () => 'fixture' } as ExecutionContext;
  const { tools } = workspaceToolsV2({ request } as unknown as ReturnType<typeof workerClients>, ctx, 'sandbox', 60_000);
  const bash = tools.find(({ name }) => name === 'bash')!;
  await bash.call({ timeout_s: 300, command: 'fixture' }, new AbortController().signal);
  expect(request).toHaveBeenCalledWith('sandbox-ctl', expect.any(String),
    expect.objectContaining({ timeoutMs: 305_000, body: expect.objectContaining({ timeout_s: 300 }) }));
});

it('redacts only the selected view text before numbering the returned lines', async () => {
  const request = vi.fn(async () => ({ exit_code: 0, stderr: '',
    stdout: JSON.stringify({ hash: 'fixture', content: 'selected\nline', total_lines: 9000 }) }));
  const sanitize = vi.fn(async (value: unknown) => value);
  const ctx = { capability: () => 'fixture' } as ExecutionContext;
  const { tools } = workspaceToolsV2({ request } as unknown as ReturnType<typeof workerClients>, ctx, 'sandbox', 60_000, sanitize);
  const view = tools.find(({ name }) => name === 'view')!;
  const result = await view.call({ path: 'fixture', start_line: 15, end_line: 16 }, new AbortController().signal);
  expect(result).toMatchObject({ content: '15\tselected\n16\tline' });
  expect(request).toHaveBeenCalledTimes(1);
  expect(sanitize).toHaveBeenCalledExactlyOnceWith('selected\nline');
});
