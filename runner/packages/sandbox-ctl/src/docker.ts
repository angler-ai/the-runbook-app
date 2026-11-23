import type Docker from 'dockerode';
import { createHash } from 'node:crypto';
import { type Duplex, Writable } from 'node:stream';
import { StringDecoder } from 'node:string_decoder';
import type { ExecRequest, ExecResponse, SandboxListenerResponse, SandboxSize } from '@runbook/runner-shared/contracts/internal';
import { HttpError } from '@runbook/runner-shared/http';
import { loadRedaction, type StreamRedactor } from '@runbook/runner-shared/redact';
import { SandboxSubnets } from './subnets.ts';
import { EXIT_MARGIN_MS, FILE_HELPER, NODE, SANDBOX_LABEL, TRUSTED_HELPER, template }
  from './template.ts';
import { DIGEST_IMAGE } from './kubernetes-template.ts';

export interface SandboxNetwork { id: string; gatewayAddress?: string; bridgeGateway?: string; subnetIndex?: number }
export interface DockerSandbox {
  id: string;
  network: SandboxNetwork;
  containerId: string;
  listener: SandboxListenerResponse;
  resources?: { vcpu: number; gib: number };
  startedAt?: number;
  env?: Record<string, string>;
  customImage?: boolean;
}

const status = (error: unknown) => (error as { statusCode?: number } | null)?.statusCode;
const unavailable = () => new HttpError(503, 'sandbox.unavailable', 'sandbox operation unavailable');
const gone = () => new HttpError(410, 'sandbox.gone', 'sandbox container is gone');
const timedOut = () => new HttpError(408, 'sandbox.exec_timeout', 'sandbox execution timed out');
const invalidFile = () => new HttpError(422, 'sandbox.invalid_file', 'file is unavailable in the workspace');
export const OUTPUT_LIMIT = 256 * 1024;
const REDACTION_TAIL = 4096;
const CLIPPED = '\n[TRUNCATED]';

export function captureEnds(redactor: StreamRedactor) {
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

export function validateExec(request: ExecRequest, setup = false): void {
  if (!Array.isArray(request.argv) || !request.argv.length || request.argv.length > 256 ||
      !request.argv.every((arg) => typeof arg === 'string' && !arg.includes('\0')) ||
      typeof request.cwd !== 'string' || !/^\/workspace(?:\/[^\0]*)?$/.test(request.cwd) ||
      request.cwd.split('/').includes('..') || !Number.isFinite(request.timeout_s) ||
      request.timeout_s <= 0 || request.timeout_s > (setup ? 600 : 300) ||
      (request.return_partial !== undefined && typeof request.return_partial !== 'boolean') ||
      (request.stdin !== undefined && (typeof request.stdin !== 'string' || Buffer.byteLength(request.stdin) > 128 * 1024))) {
    throw new HttpError(422, 'sandbox.invalid_exec', 'invalid sandbox execution request');
  }
}

export function validatePath(path: string): void {
  if (!path || path.length > 1024 ||
      path.split('/').some((part) => !part || part === '.' || part === '..' || /[\0\\]/.test(part))) throw invalidFile();
}

export class DockerSandboxBackend {
  private readonly docker: Docker;
  readonly image: string;
  private readonly gateway: string | undefined;

  private readonly subnets: SandboxSubnets;
  private readonly sandboxLabel: string;
  private trustedVolume?: Promise<string>;

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

  async requireLocalImage(image: string) {
    if (!DIGEST_IMAGE.test(image)) throw unavailable();
    const metadata = await this.docker.getImage(image).inspect().catch((error: unknown) => {
      if (status(error) === 404) throw new HttpError(503, 'sandbox.image_missing', 'approved digest must be preloaded on the Docker host');
      throw unavailable();
    });
    if (!metadata.RepoDigests?.includes(image)) throw unavailable();
    if (Object.keys(metadata.Config.Volumes ?? {}).length) {
      throw new HttpError(422, 'sandbox.image_volumes_denied', 'sandbox images cannot declare Docker volumes');
    }
    return metadata.Id;
  }

  private async seedTrustedVolume(): Promise<string> {
    const metadata = await this.docker.getImage(this.image).inspect();
    const name = 'runbook-trusted-' + createHash('sha256').update(metadata.Id).digest('hex');
    await this.docker.createVolume({ Name: name, Driver: 'local' });
    // Docker copies the trusted image directory into an empty named volume
    // when it is attached. The custom container can only mount it read-only.
    const seed = await this.docker.createContainer({
      Image: metadata.Id, Entrypoint: [TRUSTED_HELPER], Cmd: ['version'],
      User: '10001:10001', Healthcheck: { Test: ['NONE'] },
      HostConfig: { NetworkMode: 'none', ReadonlyRootfs: true, CapDrop: ['ALL'],
        Mounts: [{ Type: 'volume', Source: name, Target: '/runbook-trusted' }] },
    });
    try {
      await seed.start();
      if ((await seed.wait()).StatusCode !== 0) throw unavailable();
      return name;
    } finally {
      await seed.remove({ force: true });
    }
  }

  async customProbe(sandbox: DockerSandbox, bridge?: string) {
    return this.exec(sandbox, { argv: [TRUSTED_HELPER, 'probe-docker', bridge ?? ''], cwd: '/workspace', timeout_s: 6 },
      undefined, false, true);
  }

  async start(id: string, network: SandboxNetwork, size: SandboxSize, listener: SandboxListenerResponse,
    timeoutMs = 900_000, image = this.image, options: { custom?: boolean; setup?: boolean } = {}): Promise<DockerSandbox> {
    try {
      const customImage = options.custom ?? (image !== this.image && DIGEST_IMAGE.test(image));
      const local = customImage ? await this.requireLocalImage(image) : image;
      const opts = template(id, network.id, local, size, listener, timeoutMs, this.sandboxLabel);
      if (customImage || options.setup) {
        this.trustedVolume ??= this.seedTrustedVolume().catch((error: unknown) => {
          this.trustedVolume = undefined;
          throw error;
        });
        const volume = await this.trustedVolume;
        if (customImage) {
          opts.Entrypoint = [TRUSTED_HELPER];
          opts.Cmd = ['idle', String(timeoutMs + EXIT_MARGIN_MS)];
        }
        opts.HostConfig!.Mounts = [{ Type: 'volume', Source: volume, Target: '/runbook-trusted', ReadOnly: true,
          VolumeOptions: { NoCopy: true, Labels: {}, DriverConfig: { Name: 'local', Options: {} } } }];
      }
      const container = await this.docker.createContainer(opts).catch((error: unknown) => {
        if (status(error) === 404) throw new HttpError(503, 'sandbox.image_missing',
          'Sandbox image ' + image + ' is missing. Build or pull it before starting the runner.');
        throw error;
      });
      await container.start();
      const sandbox = { id, containerId: container.id, network, listener, customImage };
      if (customImage) return sandbox;
      const probe = await this.exec(sandbox, { argv: ['/bin/sh', '-c',
        'command -v bash >/dev/null && command -v rg >/dev/null'], cwd: '/workspace', timeout_s: 3 });
      if (probe.exit_code !== 0) throw new HttpError(503, 'sandbox.missing_tools', 'sandbox image must include Bash and ripgrep');
      return sandbox;
    } catch (error) {
      if (error instanceof HttpError && ['sandbox.missing_tools', 'sandbox.image_missing', 'sandbox.image_volumes_denied'].includes(error.code)) throw error;
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

  async exec(sandbox: DockerSandbox, request: ExecRequest, _signal?: AbortSignal, setup = false, probe = false): Promise<ExecResponse> {
    validateExec(request, setup);
    const redaction = await loadRedaction();
    const container = this.docker.getContainer(sandbox.containerId);
    let stream: Duplex | undefined;
    let timer: NodeJS.Timeout | undefined;
    const watchdog = new AbortController();
    const run = async () => {
      const command = setup
        ? [TRUSTED_HELPER, 'setup-docker', String(Math.round(request.timeout_s * 1000)), ...request.argv]
        : probe ? request.argv : ['/usr/bin/timeout', '--kill-after=1s', '--', `${request.timeout_s}s`, ...request.argv];
      const execution = await container.exec({
        Cmd: command,
        WorkingDir: request.cwd, User: '10001:10001',
        ...(sandbox.env && { Env: Object.entries(sandbox.env).map(([name, value]) => `${name}=${value}`) }),
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
          }, Math.round(request.timeout_s * 1000) + 4000);
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
    validatePath(path);
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
