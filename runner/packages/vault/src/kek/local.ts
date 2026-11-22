// The `local` KEK (design §7.3): a key file mounted only into the vault
// container, for Docker and development. Anyone with the host can decrypt;
// the UI says so. The file must be readable by its owner only.
//
// File format (JSON): {"format": "runbook-runner-local-kek/1", "key_id": hex,
// "current": n, "keys": {"1": base64, …}} with 256-bit keys. Rotation adds a
// version; older versions stay so existing wrapped keys still open.
import { randomBytes } from 'node:crypto';
import { open as openFile, readFile, rename, stat } from 'node:fs/promises';
import { composeIdentitySeed } from '@runbook/runner-shared/vault-identity';
import { canonicalJson } from '@runbook/runner-shared/binding';
import { aesGcmDecrypt, aesGcmEncrypt, DecryptError } from '../crypto/envelope.ts';
import { lengthPrefixed } from '../crypto/aad.ts';
import type { Kek, KekInfo, WrapContext } from './index.ts';

const FORMAT = 'runbook-runner-local-kek/1';

export class LocalKekError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'LocalKekError';
  }
}

interface KeyFile {
  format: string;
  key_id: string;
  current: number;
  keys: Record<string, string>;
  bootstrap_identity?: boolean;
}

async function writeKeyFile(path: string, data: KeyFile, exclusive: boolean): Promise<void> {
  const body = JSON.stringify(data, null, 2) + '\n';
  if (exclusive) {
    const handle = await openFile(path, 'wx', 0o600);
    try {
      await handle.writeFile(body);
      await handle.sync();
    } finally {
      await handle.close();
    }
    return;
  }
  const temp = `${path}.${randomBytes(6).toString('hex')}.tmp`;
  const handle = await openFile(temp, 'wx', 0o600);
  try {
    await handle.writeFile(body);
    await handle.sync();
  } finally {
    await handle.close();
  }
  await rename(temp, path);
}

/** Create a new key file (mode 0600). Refuses to overwrite an existing file. */
export async function generateLocalKeyFile(path: string): Promise<void> {
  await writeKeyFile(
    path,
    { format: FORMAT, key_id: randomBytes(8).toString('hex'), current: 1, keys: { '1': randomBytes(32).toString('base64') } },
    true,
  );
}

export class LocalKek implements Kek {
  private readonly path: string;
  private data: KeyFile;
  private keys: Map<number, Buffer>;

  private constructor(path: string, data: KeyFile, keys: Map<number, Buffer>) {
    this.path = path;
    this.data = data;
    this.keys = keys;
  }

  static async open(path: string): Promise<LocalKek> {
    let info;
    try {
      info = await stat(path);
    } catch {
      throw new LocalKekError(`local KEK file ${path} is missing or unreadable`);
    }
    if (!info.isFile()) throw new LocalKekError(`local KEK file ${path} is not a regular file`);
    if ((info.mode & 0o077) !== 0) {
      throw new LocalKekError(
        `local KEK file ${path} is readable or writable by others (mode ${(info.mode & 0o777).toString(8)}); chmod 600 it`,
      );
    }
    let data: KeyFile;
    try {
      data = JSON.parse(await readFile(path, 'utf8')) as KeyFile;
    } catch {
      throw new LocalKekError(`local KEK file ${path} is not valid JSON`);
    }
    const keys = new Map<number, Buffer>();
    const valid =
      data?.format === FORMAT &&
      typeof data.key_id === 'string' &&
      /^[0-9a-f]{16}$/.test(data.key_id) &&
      Number.isInteger(data.current) &&
      typeof data.keys === 'object' &&
      data.keys !== null;
    if (valid) {
      for (const [version, b64] of Object.entries(data.keys)) {
        const key = typeof b64 === 'string' ? Buffer.from(b64, 'base64') : Buffer.alloc(0);
        if (!/^\d+$/.test(version) || key.length !== 32) {
          throw new LocalKekError(`local KEK file ${path} has an invalid key entry`);
        }
        keys.set(Number(version), key);
      }
    }
    if (!valid || !keys.has(data.current)) {
      throw new LocalKekError(`local KEK file ${path} is not a ${FORMAT} key file`);
    }
    return new LocalKek(path, data, keys);
  }

  bootstrapIdentitySeed(): Buffer | undefined {
    if (this.data.bootstrap_identity !== true) return undefined;
    const initial = this.keys.get(1);
    if (!initial) throw new LocalKekError('local KEK is missing the Compose bootstrap key version');
    return composeIdentitySeed(initial);
  }

  private ref(version: number): string {
    return `local/${this.data.key_id}/${version}`;
  }

  private aad(kekRef: string, ctx: WrapContext): Buffer {
    return lengthPrefixed(['runbook-runner-local-kek', kekRef, canonicalJson(ctx)]);
  }

  async wrap(dek: Buffer, ctx: WrapContext): Promise<{ wrapped: Buffer; kekRef: string }> {
    const version = this.data.current;
    const key = this.keys.get(version);
    if (!key) throw new LocalKekError('local KEK has no current key');
    const kekRef = this.ref(version);
    const { nonce, ciphertext } = aesGcmEncrypt(key, dek, this.aad(kekRef, ctx));
    return { wrapped: Buffer.concat([nonce, ciphertext]), kekRef };
  }

  async unwrap(wrapped: Buffer, kekRef: string, ctx: WrapContext): Promise<Buffer> {
    const m = /^local\/([0-9a-f]{16})\/(\d+)$/.exec(kekRef);
    const key = m && m[1] === this.data.key_id ? this.keys.get(Number(m[2])) : undefined;
    if (!key || wrapped.length < 12) throw new DecryptError('data key');
    return aesGcmDecrypt(key, wrapped.subarray(0, 12), wrapped.subarray(12), this.aad(kekRef, ctx), 'data key');
  }

  /** Add a new key version and make it current (rewrites the file atomically, mode 0600). */
  async rotate(): Promise<void> {
    const next = Math.max(...this.keys.keys()) + 1;
    const key = randomBytes(32);
    const data: KeyFile = {
      ...this.data,
      current: next,
      keys: { ...this.data.keys, [String(next)]: key.toString('base64') },
    };
    await writeKeyFile(this.path, data, false);
    this.data = data;
    this.keys = new Map(this.keys).set(next, key);
  }

  describe(): KekInfo {
    return {
      provider: 'local',
      key: this.data.key_id,
      version: String(this.data.current),
      warning: 'Local key: anyone with this host can decrypt',
    };
  }
}
