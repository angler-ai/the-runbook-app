import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, readdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import Docker from 'dockerode';
import { DockerSandboxBackend } from '@runbook/runner-sandbox-ctl/docker';
import type { ExecRequest } from '@runbook/runner-shared/contracts/internal';
import type { ExecutionContext } from '@runbook/runner-worker/claim';
import { InternalError, type workerClients } from '@runbook/runner-worker/clients';
import { workspaceToolsV2 } from '@runbook/runner-worker/loop/workspace-v2';
import { workspaceTools } from '@runbook/runner-worker/loop/tools';
import type { Task } from './tasks.ts';

const maxSnapshotBytes = 2 * 1024 * 1024;
const otherEntry = '[eval: non-regular file or file outside snapshot limit]';

export async function withTempWorkspace<T>(task: Task, operation: (workspace: string, mcpData: string) => Promise<T>) {
  const root = await mkdtemp(join(tmpdir(), 'runner-eval-'));
  try {
    const workspace = join(root, 'workspace');
    await mkdir(workspace);
    for (const [name, text] of Object.entries(task.files)) await writeFile(join(workspace, name), text);
    const mcpData = join(root, 'mcp-fixture.json');
    await writeFile(mcpData, JSON.stringify(task.mcp), { mode: 0o600 });
    return await operation(workspace, mcpData);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

export async function localSnapshot(workspace: string) {
  const files: Task['files'] = {};
  const entries = await readdir(workspace, { recursive: true, withFileTypes: true });
  let remaining = maxSnapshotBytes;
  for (const entry of entries) {
    if (entry.isDirectory()) continue;
    const path = join(entry.parentPath, entry.name);
    const size = entry.isFile() ? (await stat(path)).size : Infinity;
    const value = size <= remaining ? await readFile(path, 'utf8') : otherEntry;
    Object.defineProperty(files, path.slice(workspace.length + 1), { value, enumerable: true });
    remaining -= Math.min(remaining, size);
  }
  return files;
}

const snapshotScript = String.raw`
const fs = require('node:fs');
const files = [];
function scan(dir, prefix) {
  for (const entry of fs.readdirSync(dir, {withFileTypes:true})) {
    if (files.length >= 1000) throw new Error('too many files');
    const name = prefix + entry.name;
    if (name === '.runbook-ca.pem') continue;
    if (entry.isDirectory()) scan(dir + '/' + entry.name, name + '/');
    else files.push([name, entry.isFile() ? fs.statSync(dir + '/' + entry.name).size : -1]);
  }
}
scan('/workspace', '');
process.stdout.write(JSON.stringify(files));
`;
const chunkScript = String.raw`
const fs = require('node:fs');
const [name, offset, length] = process.argv.slice(1);
const fd = fs.openSync('/workspace/' + name, fs.constants.O_NOFOLLOW | fs.constants.O_RDONLY | fs.constants.O_NONBLOCK);
const buf = Buffer.alloc(Number(length));
if (fs.readSync(fd, buf, 0, buf.length, Number(offset)) !== buf.length) process.exit(1);
process.stdout.write(buf.toString('base64'));
`;

export async function openSandbox(task: Task, signal: AbortSignal) {
  const socketPath = process.env.RUNNER_TEST_DOCKER_SOCKET ??
    process.env.DOCKER_HOST?.replace(/^unix:\/\//, '') ?? '/var/run/docker.sock';
  const docker = new Docker({ socketPath });
  const image = process.env.RUNNER_TEST_SANDBOX_IMAGE ?? 'runbook-sandbox:1';
  await docker.getImage(image).inspect().catch(() => { throw new Error(`Eval sandbox image unavailable: ${image}`); });
  const backend = new DockerSandboxBackend(docker, image, undefined, undefined, 'runbook.eval-sandbox');
  const id = `rb-eval-${randomUUID()}`;
  const network = await backend.allocateNetwork(id);
  const close = () => backend.destroy(id, network);
  try {
    const sandbox = await backend.start(id, network, 'small', {
      proxy_url: 'http://192.0.2.1:8888/', ca_pem: 'fixture-no-egress', listener_secret: 'fixture-proxy',
    });
    const exec = (argv: string[]) => backend.exec(sandbox, { argv, cwd: '/workspace', timeout_s: 20 });
    const readOutput = async (argv: string[]) => {
      const result = await exec(argv);
      if (result.exit_code !== 0) throw new Error('Sandbox fixture operation failed');
      return result.stdout;
    };
    for (const [name, text] of Object.entries(task.files)) {
      // sandbox-ctl's ordinary file API caps writes. Seed larger fixtures in
      // 32 KiB chunks to stay below both argv and tool-output limits.
      const content = Buffer.from(text);
      const init = 'require("node:fs").writeFileSync(process.argv[1], "")';
      const append = 'require("node:fs").appendFileSync(process.argv[1], Buffer.from(process.argv[2], "base64"))';
      await readOutput(['/usr/local/bin/node', '-e', init, name]);
      for (let start = 0; start < content.length; start += 32768) {
        await readOutput(['/usr/local/bin/node', '-e', append, name, content.subarray(start, start + 32768).toString('base64')]);
      }
    }
    const ctx: ExecutionContext = {
      stepRunId: task.id, claim: { id: 'fixture', generation: 1 }, signal, capability: () => 'fixture',
      registerSandbox: () => undefined, sandboxStarted: () => undefined,
    };
    const request: ReturnType<typeof workerClients>['request'] = async <T>(_role: string, path: string,
      opts: Parameters<ReturnType<typeof workerClients>['request']>[2] = {}) => {
      opts.signal?.throwIfAborted();
      try {
        if (path.endsWith('/exec')) return await backend.exec(sandbox, opts.body as ExecRequest) as T;
        const relative = path.split('/files/')[1]?.split('/').map(decodeURIComponent).join('/');
        if (!relative) throw new Error('Unknown sandbox tool route');
        if (opts.method === 'GET') return await backend.read(sandbox, relative) as T;
        await backend.write(sandbox, relative, (opts.body as { content: string }).content);
        return undefined as T;
      } catch (error) {
        if ((error as { code?: string }).code === 'sandbox.gone') throw new InternalError(410, undefined, 'sandbox.gone');
        throw error;
      }
    };
    return {
      native: workspaceToolsV2({ request } as ReturnType<typeof workerClients>, ctx, id, 20_000),
      tools: workspaceTools({ request } as ReturnType<typeof workerClients>, ctx, id, 20_000),
      close,
      async snapshot() {
        const entries: [string, number][] = JSON.parse(await readOutput(['/usr/local/bin/node', '-e', snapshotScript]));
        let remaining = maxSnapshotBytes;
        const files: [string, string][] = [];
        for (const [name, size] of entries) {
          if (size < 0 || size > remaining) { files.push([name, otherEntry]); continue; }
          remaining -= size;
          const chunks: Buffer[] = [];
          for (let offset = 0; offset < size; offset += 65536) {
            const length = Math.min(65536, size - offset);
            const output = await readOutput(['/usr/local/bin/node', '-e', chunkScript, name, String(offset), String(length)]);
            const chunk = Buffer.from(output, 'base64');
            if (chunk.length !== length) throw new Error('Sandbox snapshot was clipped');
            chunks.push(chunk);
          }
          files.push([name, Buffer.concat(chunks).toString('utf8')]);
        }
        return Object.fromEntries(files);
      },
    };
  } catch (error) {
    await close();
    throw error;
  }
}
