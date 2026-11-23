import { randomUUID } from 'node:crypto';
import { mkdir, open, readFile, rename, rm } from 'node:fs/promises';
import { dirname, isAbsolute, join } from 'node:path';
import { DeleteObjectCommand, GetObjectCommand, PutObjectCommand, S3Client } from '@aws-sdk/client-s3';
import { GoogleAuth } from 'google-auth-library';

export interface ArchiveRange {
  offset: number;
  length: number;
}

export interface ArchiveStore {
  put(key: string, data: Buffer): Promise<void>;
  get(key: string, range?: ArchiveRange): Promise<Buffer>;
  remove(key: string): Promise<void>;
}

const rangeHeader = ({ offset, length }: ArchiveRange) => `bytes=${offset}-${offset + length - 1}`;

function localStore(root: string): ArchiveStore {
  return {
    async put(key, data) {
      const path = join(root, key);
      const dir = dirname(path);
      await mkdir(dir, { recursive: true, mode: 0o700 });
      const temporary = `${path}.${randomUUID()}.tmp`;
      try {
        const file = await open(temporary, 'wx', 0o600);
        try {
          await file.writeFile(data);
          await file.sync();
        } finally {
          await file.close();
        }
        await rename(temporary, path);
        for (const folder of [dir, root]) {
          const handle = await open(folder, 'r');
          try { await handle.sync(); } finally { await handle.close(); }
        }
      } finally {
        await rm(temporary, { force: true });
      }
    },
    async get(key, range) {
      const path = join(root, key);
      if (!range) return readFile(path);
      const file = await open(path, 'r');
      try {
        const bytes = Buffer.alloc(range.length);
        const { bytesRead } = await file.read({ buffer: bytes, position: range.offset });
        return bytes.subarray(0, bytesRead);
      } finally {
        await file.close();
      }
    },
    async remove(key) { await rm(join(root, key), { force: true }); },
  };
}

function gcsStore(bucket: string, prefix: string): ArchiveStore {
  const auth = new GoogleAuth({ scopes: 'https://www.googleapis.com/auth/devstorage.read_write' });
  const objects = `/storage/v1/b/${encodeURIComponent(bucket)}/o`;
  const object = (key: string) => `${objects}/${encodeURIComponent(prefix + key)}`;
  async function request(path: string, init: RequestInit, missingOkay = false) {
    const token = await auth.getAccessToken();
    if (!token) throw new Error('Google archive credentials unavailable');
    const response = await fetch(`https://storage.googleapis.com${path}`, {
      ...init, headers: { ...init.headers, authorization: `Bearer ${token}` },
      signal: AbortSignal.timeout(120_000),
    });
    if (!response.ok && !(missingOkay && response.status === 404)) {
      await response.body?.cancel();
      throw new Error(`GCS archive request failed (HTTP ${response.status})`);
    }
    return response;
  }
  return {
    async put(key, data) {
      const query = new URLSearchParams({ uploadType: 'media', name: prefix + key });
      const response = await request(`/upload${objects}?${query}`, {
        method: 'POST', headers: { 'content-type': 'application/gzip' }, body: data as Buffer<ArrayBuffer>,
      });
      await response.body?.cancel();
    },
    async get(key, range) {
      const response = await request(`${object(key)}?alt=media`, {
        headers: range ? { range: rangeHeader(range) } : {},
      });
      return Buffer.from(await response.arrayBuffer());
    },
    async remove(key) {
      const response = await request(object(key), { method: 'DELETE' }, true);
      await response.body?.cancel();
    },
  };
}

function s3Store(bucket: string, prefix: string): ArchiveStore {
  const client = new S3Client({});
  const options = () => ({ abortSignal: AbortSignal.timeout(120_000) });
  const object = (key: string) => ({ Bucket: bucket, Key: prefix + key });
  return {
    async put(key, data) {
      await client.send(new PutObjectCommand({ ...object(key), ContentType: 'application/gzip', Body: data }), options());
    },
    async get(key, range) {
      const response = await client.send(new GetObjectCommand({
        ...object(key), Range: range && rangeHeader(range),
      }), options());
      if (!response.Body) throw new Error('S3 archive object had no body');
      return Buffer.from(await response.Body.transformToByteArray());
    },
    async remove(key) { await client.send(new DeleteObjectCommand(object(key)), options()); },
  };
}

export function archiveStore(location: string): ArchiveStore {
  if (isAbsolute(location)) return localStore(location);
  const match = /^(gs|s3):\/\/([a-z\d][a-z\d.-]*)(?:\/([^?#]*))?$/.exec(location);
  if (!match) throw new Error('RUNNER_RETENTION_ARCHIVE_STORE must be an absolute path or a gs:// or s3:// URL');
  const prefix = match[3] ? match[3].replace(/\/+$/, '') + '/' : '';
  return match[1] === 'gs' ? gcsStore(match[2]!, prefix) : s3Store(match[2]!, prefix);
}
