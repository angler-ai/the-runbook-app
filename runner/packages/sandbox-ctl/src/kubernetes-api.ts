import { readFile } from 'node:fs/promises';
import http from 'node:http';
import https from 'node:https';
import { isIP } from 'node:net';
import WebSocket from 'ws';

export class KubernetesApiError extends Error {
  readonly status: number;
  constructor(status: number) {
    super(`Kubernetes API request failed (${status})`);
    this.status = status;
  }
}

const SERVICE_ACCOUNT = '/var/run/secrets/kubernetes.io/serviceaccount';
const REQUEST_TIMEOUT = 10_000;

export class KubernetesApi {
  private readonly endpoint: string;
  private readonly namespace: string;
  private readonly auth: () => Promise<{ token?: string; ca?: Buffer }>;

  constructor(endpoint: string, namespace: string,
    auth: KubernetesApi['auth'] = async () => ({})) {
    this.endpoint = endpoint;
    this.namespace = namespace;
    this.auth = auth;
    if (!/^[a-z0-9][a-z0-9-]{0,62}$/.test(namespace)) throw new Error('invalid sandbox namespace');
  }

  static inCluster(namespace: string) {
    const host = process.env.KUBERNETES_SERVICE_HOST;
    const port = process.env.KUBERNETES_SERVICE_PORT;
    if (!host || !port || !isIP(host) || !/^\d+$/.test(port)) throw new Error('missing Kubernetes API endpoint');
    return new KubernetesApi(`https://${isIP(host) === 6 ? `[${host}]` : host}:${port}`, namespace, async () => {
      const [token, ca] = await Promise.all([
        readFile(`${SERVICE_ACCOUNT}/token`, 'utf8'), readFile(`${SERVICE_ACCOUNT}/ca.crt`),
      ]);
      return { token: token.trim(), ca };
    });
  }

  private url(pod = '', suffix = '') {
    return new URL(`/api/v1/namespaces/${this.namespace}/pods${pod ? '/' + encodeURIComponent(pod) : ''}${suffix}`, this.endpoint);
  }

  async json<T>(method: 'GET' | 'POST' | 'DELETE', pod: string, body?: object, query?: URLSearchParams,
    signal?: AbortSignal): Promise<T> {
    const url = this.url(pod);
    if (query) url.search = query.toString();
    const { token, ca } = await this.auth();
    const data = body && Buffer.from(JSON.stringify(body));
    const transport = url.protocol === 'https:' ? https : http;
    return new Promise<T>((resolve, reject) => {
      const req = transport.request(url, {
        method, ca, signal: AbortSignal.any([AbortSignal.timeout(REQUEST_TIMEOUT), ...(signal ? [signal] : [])]),
        headers: {
          ...(token && { authorization: `Bearer ${token}` }),
          ...(data && { 'content-type': 'application/json', 'content-length': data.length }),
        },
      }, (res) => {
        const code = res.statusCode ?? 500;
        if (code < 200 || code >= 300) {
          res.resume();
          reject(new KubernetesApiError(code));
          return;
        }
        let length = 0;
        const buffers: Buffer[] = [];
        res.on('data', (chunk: Buffer) => {
          length += chunk.length;
          if (length > 8 * 1024 * 1024) req.destroy(new Error('oversized Kubernetes API response'));
          else buffers.push(chunk);
        });
        res.on('error', reject);
        res.on('end', () => {
          try { resolve(JSON.parse(Buffer.concat(buffers).toString()) as T); }
          catch (error) { reject(error); }
        });
      });
      req.on('error', reject);
      req.end(data);
    });
  }

  async exec(pod: string, container: string, command: string[], input: object,
    output: (channel: 1 | 2, bytes: Buffer) => void, signal: AbortSignal): Promise<number> {
    const { token, ca } = await this.auth();
    const url = this.url(pod, '/exec');
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    url.search = new URLSearchParams({ container, stdin: 'true', stdout: 'true', stderr: 'true', tty: 'false' }).toString();
    for (const arg of command) url.searchParams.append('command', arg);
    const data = Buffer.from(JSON.stringify(input));
    if (data.length > 1024 * 1024) throw new Error('oversized sandbox helper input');
    const header = Buffer.alloc(5); // channel 0, followed by uint32 stdin length
    header.writeUInt32BE(data.length, 1);
    signal.throwIfAborted();
    return new Promise<number>((resolve, reject) => {
      const ws = new WebSocket(url, ['v4.channel.k8s.io'], {
        ca, headers: token ? { authorization: `Bearer ${token}` } : {},
        maxPayload: 1024 * 1024, perMessageDeflate: false, followRedirects: false,
      });
      let exit: number | undefined;
      const abort = () => { reject(signal.reason); ws.terminate(); };
      signal.addEventListener('abort', abort, { once: true });
      ws.on('error', (error) => { reject(error); ws.terminate(); });
      ws.once('close', () => {
        signal.removeEventListener('abort', abort);
        if (exit === undefined) reject(new Error('missing sandbox exec status'));
        else resolve(exit);
      });
      ws.once('open', () => ws.send(Buffer.concat([header, data])));
      ws.on('message', (raw, binary) => {
        try {
          if (!binary || !Buffer.isBuffer(raw) || !raw.length) throw new Error('invalid sandbox exec stream');
          const channel = raw[0];
          if (channel === 1 || channel === 2) output(channel, raw.subarray(1));
          else if (channel === 3) {
            const status = JSON.parse(raw.subarray(1).toString()) as {
              status?: string; reason?: string; details?: { causes?: { reason?: string; message?: string }[] };
            };
            if (status.status === 'Success') exit = 0;
            else if (status.reason === 'NonZeroExitCode') {
              const code = status.details?.causes?.find(({ reason }) => reason === 'ExitCode')?.message;
              if (code && /^[1-9]\d{0,2}$/.test(code) && Number(code) <= 255) exit = Number(code);
            }
            if (exit === undefined) throw new Error('invalid sandbox exec status');
            // Status is the terminal frame. Close even if the peer leaves the upgraded connection open.
            resolve(exit);
            ws.terminate();
            signal.removeEventListener('abort', abort);
          } else throw new Error('unexpected sandbox exec channel');
        } catch (error) {
          reject(error);
          ws.terminate();
        }
      });
    });
  }
}
