import type Docker from 'dockerode';
import { createHash } from 'node:crypto';
import { type Duplex, Writable } from 'node:stream';
import { StringDecoder } from 'node:string_decoder';
import type { ExecRequest, ExecResponse, SandboxListenerResponse, SandboxSize } from '@runbook/runner-shared/contracts/internal';
import { HttpError } from '@runbook/runner-shared/http';
import { loadRedaction, type StreamRedactor } from '@runbook/runner-shared/redact';
import { SandboxSubnets } from './subnets.ts';
import { FILE_HELPER, NODE, SANDBOX_LABEL, template } from './template.ts';

export interface SandboxNetwork { id: string; gatewayAddress?: string; bridgeGateway?: string; subnetIndex?: number }
export interface DockerSandbox {
  id: string;
  network: SandboxNetwork;
  containerId: string;
  listener: SandboxListenerResponse;
}

const status = (error: unknown) => (error as { statusCode?: number } | null)?.statusCode;
const unavailable = () => new HttpError(503, 'sandbox.unavailable', 'sandbox operation unavailable');
const gone = () => new HttpError(410, 'sandbox.gone', 'sandbox container is gone');
const timedOut = () => new HttpError(408, 'sandbox.exec_timeout', 'sandbox execution timed out');
const invalidFile = () => new HttpError(422, 'sandbox.invalid_file', 'file is unavailable in the workspace');
const OUTPUT_LIMIT = 256 * 1024;
const REDACTION_TAIL = 4096;
const CLIPPED = '\n[TRUNCATED]';

function captureEnds(redactor: StreamRedactor) {
  const decoder = new StringDecoder('utf8');
  const raw = createHash('sha256');
  const safe = createHash('sha256');
  const head: Buffer[] = [];
  const tail: Buffer[] = [];
  let tailBytes = 0;
  let size = 0;
  function append(text: string) {
    safe.update(text);
    const bytes = Buffer.from(text);
    if (size < OUTPUT_LIMIT) head.push(bytes.subarray(0, OUTPUT_LIMIT - size));
    size += bytes.length;
    if (!bytes.length) return;
    tail.push(bytes);
    tailBytes += bytes.length;
    while (tailBytes - tail[0]!.length >= OUTPUT_LIMIT) tailBytes -= tail.shift()!.length;
    if (tailBytes > OUTPUT_LIMIT) {
      tail[0] = tail[0]!.subarray(tailBytes - OUTPUT_LIMIT);
      tailBytes = OUTPUT_LIMIT;
    }
  }
  return {
    sink: new Writable({
      write(chunk: Buffer, _encoding, callback) {
        try {
          raw.update(chunk);
          append(redactor.push(decoder.write(chunk)));
          callback();
        } catch (error) { callback(error as Error); }
      },
    }),
    finish() {
      append(redactor.push(decoder.end()) + redactor.finish());
      return { size, redacted: raw.digest('hex') !== safe.digest('hex'), render(limit: number) {
        const start = Buffer.concat(head);
        if (size <= limit) return start.toString('utf8');
        const label = CLIPPED + '\n';
        const room = Math.floor((limit - Buffer.byteLength(label)) / 2);
        const end = Buffer.concat(tail);
        let offset = end.length - room;
        while (offset < end.length && (end[offset]! & 0xc0) === 0x80) offset++;
        return new StringDecoder('utf8').write(start.subarray(0, room)) + label + end.subarray(offset).toString('utf8');
      } };
    },
    dispose: () => redactor.dispose(),
  };
}

export class DockerSandboxBackend {
  private readonly docker: Docker;
  readonly image: string;
  private readonly gateway: string | undefined;

  private readonly subnets: SandboxSubnets;
  private readonly sandboxLabel: string;

  constructor(docker: Docker, image: string, gateway?: string, subnetPool?: string, sandboxLabel = SANDBOX_LABEL) {
    this.docker = docker;
    this.image = image;
    this.gateway = gateway;
    this.subnets = new SandboxSubnets(subnetPool);
    this.sandboxLabel = sandboxLabel;
  }

  async allocateNetwork(id: string): Promise<SandboxNetwork> {
    const conflicts = [];
    let network: Docker.Network | undefined;
    let allocation: ReturnType<SandboxSubnets['take']>;
    try {
      for (;;) {
        allocation = this.subnets.take();
        try {
          network = await this.docker.createNetwork({
            Name: id, Driver: 'bridge', Internal: true, EnableIPv6: false,
            Options: { 'com.docker.network.bridge.inhibit_ipv4': 'true' },
            IPAM: { Config: [{ Subnet: allocation.subnet, Gateway: allocation.gateway }] },
            Labels: { [this.sandboxLabel]: '1' },
          });
          break;
        } catch (error) {
          conflicts.push(allocation.index);
          if (!/overlap/i.test(String((error as { reason?: string })?.reason ?? error))) throw unavailable();
        }
      }
    } finally {
      for (const index of conflicts) this.subnets.release(index);
    }
    const result: SandboxNetwork = { id: network.id, bridgeGateway: allocation.gateway, subnetIndex: allocation.index };
    try {
      if (this.gateway) {
        await network.connect({ Container: this.gateway });
        const gateway = await this.docker.getContainer(this.gateway).inspect();
        const address = Object.values(gateway.NetworkSettings.Networks)
          .find((endpoint) => endpoint.NetworkID === network.id)?.IPAddress;
        if (!address) throw unavailable();
        result.gatewayAddress = address;
      }
      return result;
    } catch {
      await this.destroy(id, result).catch(() => undefined);
      throw unavailable();
    }
  }

  async start(id: string, network: SandboxNetwork, size: SandboxSize, listener: SandboxListenerResponse, timeoutMs = 900_000): Promise<DockerSandbox> {
    try {
      const opts = template(id, network.id, this.image, size, listener, timeoutMs, this.sandboxLabel);
      const container = await this.docker.createContainer(opts).catch((error: unknown) => {
        if (status(error) === 404) throw new HttpError(503, 'sandbox.image_missing',
          'Sandbox image ' + this.image + ' is missing. Build or pull it before starting the runner.');
        throw error;
      });
      await container.start();
      const sandbox = { id, containerId: container.id, network, listener };
      const probe = await this.exec(sandbox, { argv: ['/bin/sh', '-c',
        'command -v bash >/dev/null && command -v rg >/dev/null'], cwd: '/workspace', timeout_s: 3 });
      if (probe.exit_code !== 0) throw new HttpError(503, 'sandbox.missing_tools', 'sandbox image must include Bash and ripgrep');
      return sandbox;
    } catch (error) {
      if (error instanceof HttpError && ['sandbox.missing_tools', 'sandbox.image_missing'].includes(error.code)) throw error;
      throw unavailable();
    }
  }

  async removeOrphans(): Promise<string[]> {
    const filters = { label: [`${this.sandboxLabel}=1`] };
    try {
      const [containers, networks] = await Promise.all([
        this.docker.listContainers({ all: true, filters }),
        this.docker.listNetworks({ filters }),
      ]);
      await Promise.all(containers.map(({ Id }) => this.removeContainer(Id)));
      for (const network of networks) await this.destroy(network.Name, { id: network.Id });
      return [...new Set([...networks.map(({ Name }) => Name), ...containers.flatMap(({ Names }) =>
        Names.map((name) => name.replace(/^\//, '')))])].filter((name) => /^rb-[a-f0-9]{64}$/.test(name));
    } catch {
      throw unavailable();
    }
  }

  private async removeContainer(id: string): Promise<void> {
    const container = this.docker.getContainer(id);
    const info = await container.inspect().catch((error: unknown) => {
      if (status(error) !== 404) throw error;
    });
    if (info?.Config.Labels?.[this.sandboxLabel] === '1') {
      await container.remove({ force: true, v: true }).catch((error: unknown) => {
        if (status(error) !== 404) throw error;
      });
    }
  }

  async destroy(id: string, network: SandboxNetwork): Promise<void> {
    try {
      await this.removeContainer(id);
      const bridge = this.docker.getNetwork(network.id);
      if (this.gateway) {
        const info = await bridge.inspect().catch((error: unknown) => {
          if (status(error) !== 404) throw error;
        });
        if (!info) {
          if (network.subnetIndex !== undefined) {
            this.subnets.release(network.subnetIndex);
            delete network.subnetIndex;
          }
          return;
        }
        const endpoint = Object.entries(info.Containers ?? {}).find(([key, entry]) => key === this.gateway || entry.Name === this.gateway);
        if (endpoint) await bridge.disconnect({ Container: endpoint[0], Force: true });
      }
      await bridge.remove().catch((error: unknown) => {
        if (status(error) !== 404) throw error;
      });
      if (network.subnetIndex !== undefined) {
        this.subnets.release(network.subnetIndex);
        delete network.subnetIndex;
      }
    } catch {
      throw unavailable();
    }
  }

  async exec(sandbox: DockerSandbox, request: ExecRequest): Promise<ExecResponse> {
    if (!Array.isArray(request.argv) || !request.argv.length || request.argv.length > 256 ||
        !request.argv.every((arg) => typeof arg === 'string' && !arg.includes('\0')) ||
        typeof request.cwd !== 'string' || !/^\/workspace(?:\/[^\0]*)?$/.test(request.cwd) ||
        request.cwd.split('/').includes('..') || !Number.isFinite(request.timeout_s) ||
        request.timeout_s <= 0 || request.timeout_s > 300 ||
        (request.return_partial !== undefined && typeof request.return_partial !== 'boolean') ||
        (request.stdin !== undefined && (typeof request.stdin !== 'string' || Buffer.byteLength(request.stdin) > 128 * 1024))) {
      throw new HttpError(422, 'sandbox.invalid_exec', 'invalid sandbox execution request');
    }
    const redaction = await loadRedaction();
    const container = this.docker.getContainer(sandbox.containerId);
    let stream: Duplex | undefined;
    let timer: NodeJS.Timeout | undefined;
    const watchdog = new AbortController();
    const run = async () => {
      // GNU timeout supervises this Docker exec's process group inside the sandbox.
      // The controller watchdog bounds the Docker transport independently.
      const execution = await container.exec({
        Cmd: ['/usr/bin/timeout', '--kill-after=1s', '--', `${request.timeout_s}s`, ...request.argv],
        WorkingDir: request.cwd, User: '10001:10001',
        AttachStdout: true, AttachStderr: true, AttachStdin: request.stdin !== undefined, Tty: false,
      });
      watchdog.signal.throwIfAborted();
      const attached = await execution.start({ hijack: true, stdin: request.stdin !== undefined });
      stream = attached as Duplex;
      if (watchdog.signal.aborted) {
        stream.destroy();
        watchdog.signal.throwIfAborted();
      }
      const stdout: Buffer[] = [];
      const stderr: Buffer[] = [];
      const clipped = { stdout: false, stderr: false };
      const room = { stdout: OUTPUT_LIMIT + REDACTION_TAIL, stderr: OUTPUT_LIMIT + REDACTION_TAIL };
      const collect = (field: 'stdout' | 'stderr', destination: Buffer[]) => new Writable({
        write(chunk: Buffer, _encoding, callback) {
          const retained = Math.min(room[field], chunk.length);
          if (retained > 0) destination.push(Buffer.from(chunk.subarray(0, retained)));
          room[field] -= retained;
          clipped[field] ||= retained < chunk.length;
          callback();
        },
      });
      const finished = new Promise<void>((resolve, reject) => {
        attached.once('end', resolve);
        attached.once('close', resolve);
        attached.once('error', reject);
      });
      const outEnds = request.return_partial ? captureEnds(redaction.stream([sandbox.listener.listener_secret])) : undefined;
      const errEnds = request.return_partial ? captureEnds(redaction.stream([sandbox.listener.listener_secret])) : undefined;
      let result: Awaited<ReturnType<typeof execution.inspect>>;
      let partial: Pick<ExecResponse, 'stdout' | 'stderr' | 'stdout_redacted' | 'truncated'> | undefined;
      try {
        this.docker.modem.demuxStream(attached, outEnds?.sink ?? collect('stdout', stdout), errEnds?.sink ?? collect('stderr', stderr));
        if (request.stdin !== undefined) attached.end(request.stdin);
        await finished;
        result = await execution.inspect();
        if (outEnds && errEnds) {
          const out = outEnds.finish();
          const err = errEnds.finish();
          const outBudget = OUTPUT_LIMIT - Math.min(err.size, OUTPUT_LIMIT / 2);
          const stdout = out.render(outBudget);
          const stderr = err.render(OUTPUT_LIMIT - Buffer.byteLength(stdout));
          partial = { stdout, stderr, ...(out.redacted && { stdout_redacted: true }),
            ...(out.size + err.size > OUTPUT_LIMIT && { truncated: true }) };
        }
      } finally {
        outEnds?.dispose();
        errEnds?.dispose();
      }
      if (result.Running || result.ExitCode === null) throw unavailable();
      const timedOutExit = result.ExitCode === 124 || result.ExitCode === 137;
      if (timedOutExit && !request.return_partial) throw timedOut();
      if (partial) return { exit_code: result.ExitCode, ...(timedOutExit && { timed_out: true }), ...partial };
      const safe = (output: Buffer[]) => {
        const raw = Buffer.concat(output).toString('utf8');
        const text = redaction.redact(raw, [sandbox.listener.listener_secret]);
        return { text, redacted: raw !== text };
      };
      const out = safe(stdout);
      const err = safe(stderr);
      let remaining = OUTPUT_LIMIT;
      function limit(field: keyof typeof clipped, value: string, reserve: number) {
        const bytes = Buffer.from(value, 'utf8');
        const space = remaining - reserve;
        let text = value;
        if (clipped[field] || bytes.length > space) {
          const size = Math.max(0, space - Buffer.byteLength(CLIPPED));
          text = new StringDecoder('utf8').write(bytes.subarray(0, size)) + CLIPPED;
        }
        remaining -= Buffer.byteLength(text);
        return text;
      }
      return { exit_code: result.ExitCode, ...(timedOutExit && { timed_out: true }),
        stdout: limit('stdout', out.text, err.text ? Buffer.byteLength(CLIPPED) : 0),
        stderr: limit('stderr', err.text, 0),
        ...(out.redacted && { stdout_redacted: true }) };
    };
    try {
      return await Promise.race([
        run(),
        new Promise<never>((_, reject) => {
          timer = setTimeout(() => {
            watchdog.abort();
            stream?.destroy();
            reject(timedOut());
          }, request.timeout_s * 1000 + 4000);
        }),
      ]);
    } catch (error) {
      const info = await container.inspect().catch((failure: unknown) => {
        if (status(failure) === 404) throw gone();
        throw unavailable();
      });
      if (!info.State.Running) throw gone();
      throw error instanceof HttpError ? error : unavailable();
    } finally {
      clearTimeout(timer);
    }
  }

  private async file(sandbox: DockerSandbox, operation: 'read' | 'write', path: string, content?: string) {
    if (!path || path.length > 1024 ||
        path.split('/').some((part) => !part || part === '.' || part === '..' || /[\0\\]/.test(part))) throw invalidFile();
    const result = await this.exec(sandbox, {
      argv: [NODE, '-e', FILE_HELPER, operation, path], cwd: '/workspace', timeout_s: 10,
      ...(content === undefined ? {} : { stdin: content }),
    });
    if (result.exit_code !== 0) throw invalidFile();
    return { content: result.stdout, redacted: result.stdout_redacted === true };
  }

  read(sandbox: DockerSandbox, path: string) {
    return this.file(sandbox, 'read', path);
  }

  async write(sandbox: DockerSandbox, path: string, content: string): Promise<void> {
    await this.file(sandbox, 'write', path, content);
  }
}
