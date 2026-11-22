import { createHash, randomUUID } from 'node:crypto';
import { posix } from 'node:path';
import { StringDecoder } from 'node:string_decoder';
import type { ExecRequest, ExecResponse } from '@runbook/runner-shared/contracts/internal';
import type { ExecutionContext } from '../claim.ts';
import { InternalError, type workerClients } from '../clients.ts';
import { StepStopped } from './limits.ts';
import type { AgentTool } from './tools.ts';

const resultRoot = '/runbook-results';
const state = resultRoot + '/environment';
const node = '/usr/local/bin/node';
const bash = '/bin/bash';
const string = { type: 'string', minLength: 1 };
const integer = { type: 'integer', minimum: 1 };
const schema = (properties: Record<string, object>, required = Object.keys(properties)) =>
  ({ type: 'object', properties, required, additionalProperties: false });
const digest = (value: string) => createHash('sha256').update(value).digest('hex');

const shellSetup = String.raw`
state=$1
runbook_initial_exports=$(compgen -e)
if [[ -f "$state" ]]; then source "$state"; fi
`;

const foreground = shellSetup + String.raw`
save() {
  local result=$?
  (
    umask 077
    printf 'cd -- %q 2>/dev/null || cd /workspace || exit\n' "$PWD"
    while IFS= read -r name; do
      declaration=$(declare -p "$name" 2>/dev/null) || declaration=
      if [[ ! "$declaration" =~ ^declare\ -[^[:space:]]*x ]]; then
        printf 'unset -- %q\n' "$name"
      fi
    done <<< "$runbook_initial_exports"
    export -p
  ) > "$state.next" && /bin/mv -f -- "$state.next" "$state"
  return "$result"
}
trap save EXIT
eval "$2" </dev/null
`;

const background = shellSetup + String.raw`
eval "$2" </dev/null
`;

const helper = String.raw`
const fs = require('node:fs');
const { dirname } = require('node:path');
const [action, path, ...args] = process.argv.slice(1);
const read = (filename, maxBytes = 2097152) => {
  const fd = fs.openSync(filename, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK);
  try {
    const stat = fs.fstatSync(fd);
    if (!stat.isFile()) throw new Error('Expected a regular text file.');
    if (stat.size > maxBytes) throw new Error(maxBytes === 131072
      ? 'This file exceeds the 128 KiB edit limit.' : 'Expected a text file up to 2 MiB.');
    return fs.readFileSync(fd, 'utf8');
  } finally { fs.closeSync(fd); }
};
const print = (value) => process.stdout.write(JSON.stringify(value));
try {
  const root = path.startsWith('/runbook-results') ? '/runbook-results' : '/workspace';
  if (path !== root) {
    const parent = action === 'create' ? root : fs.realpathSync(dirname(path));
    if (parent !== root && !parent.startsWith(root + '/')) throw new Error('Path leaves its root.');
  }
  if (action === 'view') {
    if (fs.lstatSync(path).isDirectory()) {
      const names = [];
      function list(dir, prefix, depth) {
        for (const item of fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
          if (item.name === '.runbook-ca.pem' || names.length >= 200) continue;
          const name = prefix + item.name;
          names.push(name + (item.isDirectory() ? '/' : ''));
          if (depth && item.isDirectory()) list(dir + '/' + item.name, name + '/', depth - 1);
        }
      }
      list(path, '', 1);
      print({ entries: names });
    } else {
      const [start, end, column] = args.map(Number);
      const text = read(path);
      const hash = require('node:crypto').createHash('sha256').update(text).digest('hex');
      const lines = text.split('\n');
      const selected = lines.slice(start - 1, end);
      if (selected.length === 1 && (column > 1 || selected[0].length > 8000)) {
        const line = selected[0];
        let stop = Math.min(line.length, column - 1 + 8000);
        // Do not split a UTF-16 surrogate pair across pages.
        if (line.charCodeAt(stop) >= 0xdc00 && line.charCodeAt(stop) <= 0xdfff) stop--;
        print({ hash, content: line.slice(column - 1, stop), total_lines: lines.length,
          next_column: stop < line.length ? stop + 1 : undefined });
        return;
      }
      const visible = selected.join('\n');
      const result = { hash, content: visible, total_lines: lines.length,
        truncated: false, next_line: selected.length === 2000 && end < lines.length ? end + 1 : undefined };
      // Keep the JSON envelope comfortably within the sandbox transport limit.
      if (Buffer.byteLength(JSON.stringify(result)) > 224 * 1024) {
        result.content = new (require('node:string_decoder').StringDecoder)('utf8').write(Buffer.from(visible).subarray(0, 38000));
        result.truncated = true;
      }
      print(result);
    }
  } else if (action === 'raw') {
    process.stdout.write(read(path, 131072));
  } else if (action === 'replace') {
    const text = read(path);
    const hash = require('node:crypto').createHash('sha256').update(text).digest('hex');
    if (hash !== args[0]) throw new Error('File changed during the edit. View it and try again.');
    const temp = path + '.runbook-edit-' + require('node:crypto').randomUUID();
    try {
      fs.writeFileSync(temp, fs.readFileSync(0), { flag: 'wx', mode: fs.statSync(path).mode & 0o777 });
      if (read(path) !== text) throw new Error('File changed during the edit. View it and try again.');
      fs.renameSync(temp, path);
      print({ ok: true });
    } finally { fs.rmSync(temp, { force: true }); }
  } else if (action === 'create') {
    if (path === root) throw new Error('Expected a file path inside the workspace.');
    const parts = path.slice(root.length + 1).split('/');
    const filename = parts.pop();
    const flags = fs.constants.O_RDONLY | fs.constants.O_DIRECTORY | fs.constants.O_NOFOLLOW;
    let parent = fs.openSync(root, flags);
    try {
      for (const part of parts) {
        const dir = '/proc/self/fd/' + parent + '/' + part;
        try { fs.mkdirSync(dir); } catch (error) { if (error.code !== 'EEXIST') throw error; }
        const next = fs.openSync(dir, flags);
        fs.closeSync(parent);
        parent = next;
      }
      fs.writeFileSync('/proc/self/fd/' + parent + '/' + filename, fs.readFileSync(0), {
        flag: fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK |
          (args[0] === 'true' ? fs.constants.O_TRUNC : fs.constants.O_EXCL), mode: 0o644,
      });
    } finally { fs.closeSync(parent); }
    print({ ok: true });
  } else if (action === 'glob') {
    const { spawnSync } = require('node:child_process');
    const result = spawnSync('rg', ['--files', '--hidden', '--no-ignore', '--glob', '!.runbook-ca.pem', '--glob', args[0], '--null', '.'],
      { cwd: path, encoding: 'utf8', maxBuffer: 2 * 1024 * 1024 });
    if (result.error || result.status > 1) throw new Error('ripgrep could not list paths.');
    let allowed;
    if (args[1] === 'true') {
      const listed = spawnSync('rg', ['--files', '--hidden', '--null', '.'],
        { cwd: path, encoding: 'utf8', maxBuffer: 2 * 1024 * 1024 });
      if (listed.error || listed.status > 1) throw new Error('ripgrep could not list unignored paths.');
      allowed = new Set(listed.stdout.split('\0'));
    }
    const matches = result.stdout.split('\0').filter((name) => name && (!allowed || allowed.has(name))).map((name) => {
      try { return [name.replace(/^\.\//, ''), fs.statSync(path + '/' + name).mtimeMs]; } catch { return null; }
    }).filter(Boolean);
    matches.sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
    print({ paths: matches.slice(0, 200).map(([name]) => name), truncated: matches.length > 200 });
  }
} catch (error) {
  const message = error.code === 'EEXIST' ? 'File exists. Set overwrite to true to replace it.' :
    error.code === 'ENOENT' ? 'Path not found: ' + path : error.message;
  if (action === 'raw') {
    process.stderr.write(message);
    process.exitCode = 1;
  } else print({ error: message });
}
`;

const superviseJob = String.raw`
const fs = require('node:fs');
const { spawn } = require('node:child_process');
const [path, program, state, command] = process.argv.slice(1);
const fd = fs.openSync(path + '.log', 'wx', 0o600);
const child = spawn('/bin/bash', ['-c', program, 'runbook-background', state, command, path],
  { stdio: ['ignore', 'pipe', 'pipe'] });
let room = 512 * 1024;
let clipped = false;
function collect(chunk) {
  const count = Math.min(room, chunk.length);
  if (count) fs.writeSync(fd, chunk, 0, count);
  room -= count;
  if (count < chunk.length && !clipped) {
    clipped = true;
    fs.writeSync(fd, '\n[Background log limit reached]\n');
  }
}
child.stdout.on('data', collect);
child.stderr.on('data', collect);
child.on('error', () => { process.exitCode = 1; });
child.on('close', (code) => {
  fs.closeSync(fd);
  fs.writeFileSync(path + '.status', String(code ?? 1));
});
`;

const startJob = String.raw`
const fs = require('node:fs');
const { spawn } = require('node:child_process');
const [path, program, state, command] = process.argv.slice(1);
const child = spawn(process.execPath, ['-e', ${JSON.stringify(superviseJob)}, path, program, state, command],
  { detached: true, stdio: 'ignore' });
child.once('error', () => { process.exitCode = 1; });
child.once('spawn', () => {
  // Linux proc stat field 22 is the immutable process start time.
  try {
    const stat = fs.readFileSync('/proc/' + child.pid + '/stat', 'utf8');
    fs.writeFileSync(path + '.pid', child.pid + ' ' + stat.slice(stat.lastIndexOf(')') + 2).split(' ')[19]);
  } catch { /* The command already exited. Its status file is authoritative. */ }
  child.unref();
});
`;

const pollJob = String.raw`
const fs = require('node:fs');
const [path, offset, action] = process.argv.slice(1);
function group() {
  const [pid, birth] = fs.readFileSync(path + '.pid', 'utf8').split(' ');
  if (!/^\d+$/.test(pid) || Number(pid) < 2) return;
  const stat = fs.readFileSync('/proc/' + pid + '/stat', 'utf8');
  const fields = stat.slice(stat.lastIndexOf(')') + 2).split(' ');
  if (fields[19] === birth && fields[2] === pid) return Number(pid);
}
if (action === 'kill') {
  let pid;
  try { pid = group(); } catch {}
  if (pid) process.kill(-pid, 'SIGKILL');
  process.stdout.write(JSON.stringify({ stopped: Boolean(pid) }));
} else {
  let fd;
  try { fd = fs.openSync(path + '.log', 'r'); } catch (error) { if (error.code !== 'ENOENT') throw error; }
  const data = Buffer.alloc(60000);
  const count = fd === undefined ? 0 : fs.readSync(fd, data, 0, data.length, Number(offset));
  if (fd !== undefined) fs.closeSync(fd);
  let exit_code;
  try { exit_code = Number(fs.readFileSync(path + '.status', 'utf8')); } catch {}
  let running;
  if (exit_code === undefined) { try { running = Boolean(group()); } catch { running = false; } }
  process.stdout.write(JSON.stringify({ output: data.subarray(0, count).toString(), offset: Number(offset) + count,
    running: running ?? false, exit_code }));
}
`;

export function workspaceToolsV2(clients: ReturnType<typeof workerClients>, ctx: ExecutionContext, id: string,
  toolMs: number, sanitize?: (value: unknown) => Promise<unknown>) {
  const base = `/internal/sandboxes/${id}/exec`;
  const viewed = new Map<string, { hash: string; redacted: boolean }>();
  const jobs = new Map<string, number>();
  let nextJob = 0;
  const paths = new Set<string>();
  const exec = async (argv: string[], signal: AbortSignal, opts: Partial<ExecRequest> = {}) => {
    try {
      const timeout_s = opts.timeout_s ?? Math.min(toolMs / 1000, 300);
      return await clients.request<ExecResponse>('sandbox-ctl', base, {
        body: { argv, cwd: '/workspace', ...opts, timeout_s } satisfies ExecRequest,
        capability: ctx.capability(), signal, timeoutMs: timeout_s * 1000 + 5000,
      });
    } catch (error) {
      if (error instanceof InternalError && error.code === 'sandbox.gone') throw new StepStopped('sandbox_gone');
      throw error;
    }
  };
  function pathOf(value: unknown, mode: 'read' | 'write') {
    if (typeof value !== 'string' || value.includes('\0') || value.split('/').includes('..')) throw new Error('Invalid path.');
    const path = value.replace(/\/+$/, '').replace(/^\.\//, '');
    if (mode === 'read' && paths.has(path)) return path;
    const relative = path === '.' || path === '/workspace' ? '' : path.replace(/^\/workspace\//, '');
    if (relative.startsWith('/') || relative.split('/').includes('.runbook-ca.pem')) throw new Error('Path is outside the accessible workspace.');
    return posix.join('/workspace', relative);
  }
  const json = async (script: string, args: string[], signal: AbortSignal, stdin?: string) => {
    const result = await exec([node, '-e', script, ...args], signal, { stdin });
    if (result.exit_code !== 0) return { error: 'Sandbox operation failed.' } as Record<string, unknown>;
    try { return { ...JSON.parse(result.stdout), ...(result.stdout_redacted && { redacted: true }) } as Record<string, unknown>; }
    catch { return { error: 'Sandbox output exceeded the structured response limit.' }; }
  };
  const tools: AgentTool[] = [{
    name: 'bash', description: 'Run a fresh Bash command with the current working directory and exports. Defaults to a 120-second timeout.',
    schema: schema({ command: string, timeout_s: { ...integer, maximum: 300 } }, ['command']),
    timeoutMs: (args) => 5500 + Math.min(300, typeof args.timeout_s === 'number' ? args.timeout_s : 120) * 1000,
    async call(args, signal) {
      const timeout_s = Math.min(300, typeof args.timeout_s === 'number' ? args.timeout_s : 120);
      const result = await exec([bash, '-c', foreground, 'runbook-bash', state, String(args.command)], signal,
        { timeout_s, return_partial: true });
      return { ...result, ...(result.timed_out && { timeout_s, warning: 'Outcome unknown: check before retrying.' }) };
    },
  }, {
    name: 'bash_background', description: 'Start a Bash command in a separate background process group. Returns a job ID.',
    schema: schema({ command: string }),
    async call(args, signal) {
      if (nextJob >= 32) return { error: 'This step has reached the limit of 32 background jobs.' };
      const job_id = String(++nextJob);
      const path = resultRoot + '/job-' + job_id;
      const result = await exec([node, '-e', startJob, path, background, state, String(args.command)], signal);
      if (result.exit_code !== 0) return { error: 'Could not start background command.' };
      jobs.set(job_id, 0);
      return { job_id };
    },
  }, {
    name: 'bash_output', description: 'Read new output from a background job.',
    schema: schema({ job_id: string }),
    async call(args, signal) {
      const job = String(args.job_id);
      const offset = jobs.get(job);
      if (offset === undefined) return { error: 'Unknown job.' };
      const result = await json(pollJob, [resultRoot + '/job-' + job, String(offset), 'output'], signal);
      if (typeof result.offset === 'number') jobs.set(job, Math.max(jobs.get(job) ?? 0, result.offset));
      return result;
    },
  }, {
    name: 'bash_kill', description: 'Stop the process group for a background job.',
    schema: schema({ job_id: string }),
    call(args, signal) {
      return jobs.has(String(args.job_id)) ? json(pollJob, [resultRoot + '/job-' + args.job_id, '0', 'kill'], signal) :
        Promise.resolve({ error: 'Unknown job.' });
    },
  }, {
    name: 'view', description: 'View a text file with numbered lines, optionally by inclusive range, or list a directory two levels deep.',
    parallel: true, schema: schema({ path: string, start_line: integer, end_line: integer, start_column: integer }, ['path']),
    async call(args, signal) {
      const path = pathOf(args.path, 'read');
      const start = Number(args.start_line ?? 1);
      const column = Number(args.start_column ?? 1);
      const end = Math.min(Number(args.end_line ?? (args.start_column === undefined ? start + 1999 : start)), start + 1999);
      if (args.start_column !== undefined && end !== start) return { error: 'A start_column requires a single line.' };
      if (end < start) return { error: 'The end line precedes the start line.' };
      const result = await json(helper, ['view', path, String(start), String(end), String(column)], signal);
      if (typeof result.hash === 'string' && typeof result.content === 'string') {
        // Redact the selected text before adding line numbers.
        const safe = sanitize ? await sanitize(result.content) : result.content;
        const redacted = result.redacted === true || safe !== result.content;
        viewed.set(path, { hash: result.hash, redacted });
        const { hash: _hash, content: _content, ...visible } = result;
        const content = typeof safe === 'string' && (typeof result.total_lines !== 'number' || start <= result.total_lines) ? safe.split('\n').map((line, offset) => `${start + offset}\t${line}`).join('\n') : '';
        return { ...visible, content, redacted };
      }
      return result;
    },
  }, {
    name: 'create', description: 'Write a workspace file. An existing file requires overwrite: true.',
    schema: schema({ path: string, content: { type: 'string' }, overwrite: { type: 'boolean' } }, ['path', 'content']),
    async call(args, signal) {
      const path = pathOf(args.path, 'write');
      const content = String(args.content);
      if (Buffer.byteLength(content) > 128 * 1024) return { error: 'This file exceeds the 128 KiB create limit.' };
      const result = await json(helper, ['create', path, String(args.overwrite === true)], signal, content);
      if (result.ok) viewed.set(path, { hash: digest(content), redacted: false });
      return result;
    },
  }, {
    name: 'str_replace', description: 'Replace one exact match in a workspace file already viewed or created.',
    schema: schema({ path: string, old_str: string, new_str: { type: 'string' } }),
    async call(args, signal) {
      const path = pathOf(args.path, 'write');
      const prior = viewed.get(path);
      if (!prior) return { error: 'View or create this file before editing.' };
      if (prior.redacted) return { error: 'The file contains redacted values. Editing an incomplete representation could corrupt it.' };
      const raw = await exec([node, '-e', helper, 'raw', path], signal);
      if (raw.exit_code !== 0) return { error: raw.stderr || 'Unable to read the file for editing.' };
      const text = raw.stdout;
      if (raw.stdout_redacted || (sanitize && await sanitize(text) !== text)) {
        viewed.set(path, { ...prior, redacted: true });
        return { error: 'The file contains redacted values. Editing an incomplete representation could corrupt it.' };
      }
      const from = String(args.old_str);
      const to = String(args.new_str);
      const offset = text.indexOf(from);
      if (offset < 0 || text.indexOf(from, offset + 1) >= 0) return { error: 'old_str must match exactly once.' };
      const edited = text.slice(0, offset) + to + text.slice(offset + from.length);
      if (Buffer.byteLength(edited) > 128 * 1024) return { error: 'The edited file would exceed the 128 KiB edit limit.' };
      const currentHash = digest(text);
      const result = await json(helper, ['replace', path, currentHash], signal, edited);
      if (!result.ok) return result;
      viewed.set(path, { hash: digest(edited), redacted: false });
      const line = edited.slice(0, offset).split('\n').length;
      return { ok: true, file_changed_since_view: currentHash !== prior.hash, line,
        context: edited.split('\n').slice(Math.max(0, line - 3), line + to.split('\n').length + 1).join('\n') };
    },
  }, {
    name: 'glob', description: 'Find workspace paths by glob pattern, newest first.', parallel: true,
    schema: schema({ pattern: string, path: string, respect_ignore: { type: 'boolean' } }, ['pattern']),
    call: (args, signal) => json(helper, ['glob', pathOf(args.path ?? '.', 'read'), String(args.pattern), String(args.respect_ignore === true)], signal),
  }, {
    name: 'grep', description: 'Search with ripgrep, returning filenames and line numbers.', parallel: true,
    schema: schema({ pattern: string, path: string, glob: string, ignore_case: { type: 'boolean' },
      context: { type: 'integer', minimum: 0, maximum: 20 } }, ['pattern']),
    async call(args, signal) {
      const path = pathOf(args.path ?? '.', 'read');
      const match = await exec(['rg', '--color', 'never', '--line-number', '--with-filename', '--max-count', '100',
        '--max-columns', '1000', ...(args.ignore_case === true ? ['--ignore-case'] : []),
        ...(typeof args.context === 'number' ? ['--context', String(args.context)] : []), '--glob', '!.runbook-ca.pem', ...(args.glob ? ['--glob', String(args.glob)] : []),
        '-e', String(args.pattern), '--', path], signal);
      return match.exit_code === 1 ? { matches: '' } : match.exit_code === 0
        ? { matches: match.stdout } : { error: 'ripgrep could not search the given path or pattern.', stderr: match.stderr };
    },
  }];
  async function store(path: string, value: string, signal: AbortSignal) {
    const bytes = Buffer.from(value);
    const decoder = new StringDecoder('utf8');
    const temporary = posix.dirname(path) + '/.pending-' + randomUUID();
    try {
      for (let i = 0; i < Math.max(1, bytes.length); i += 96 * 1024) {
        const piece = decoder.write(bytes.subarray(i, i + 96 * 1024));
        const result = await exec([node, '-e', 'require("node:fs").appendFileSync(process.argv[1], require("node:fs").readFileSync(0), { mode: 0o600 })',
          temporary], signal, { stdin: piece });
        if (result.exit_code !== 0) throw new StepStopped('result_storage_failed');
      }
      // Both tmpfs mounts are writable, but rename is only atomic within one.
      const result = await exec([node, '-e', 'require("node:fs").renameSync(process.argv[1], process.argv[2])',
        temporary, path], signal);
      if (result.exit_code !== 0) throw new StepStopped('result_storage_failed');
    } catch (error) {
      if (!signal.aborted) await exec([node, '-e', 'require("node:fs").rmSync(process.argv[1], { force: true })',
        temporary], signal).catch(() => undefined);
      throw error;
    }
    paths.add(path);
    return path;
  }
  return { tools: tools.map((tool) => ({ ...tool, source: 'Sandbox' })),
    storeResult: (id: string, value: string, signal: AbortSignal) => {
      const name = /^call [1-9]\d*$/.test(id) ? id.replace(' ', '-') : 'result-' + digest(id).slice(0, 24);
      return store(resultRoot + '/' + name + '.txt', value, signal);
    },
    storeInput: (value: string, signal: AbortSignal) => store('/workspace/.runbook-input.json', value, signal),
  };
}
