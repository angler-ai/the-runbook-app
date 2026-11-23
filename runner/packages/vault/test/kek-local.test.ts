import { chmodSync, mkdtempSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { DecryptError } from '../src/crypto/envelope.ts';
import { generateLocalKeyFile, LocalKek, LocalKekError } from '../src/kek/local.ts';

const dir = () => mkdtempSync(join(tmpdir(), 'rb-kek-'));
const ctx = { org_id: 'org_1', credential_id: 'cr_1' };

describe('local KEK', () => {
  it('local_kek_refuses_world_readable_key_file', async () => {
    const file = join(dir(), 'kek.json');
    await generateLocalKeyFile(file);
    expect(statSync(file).mode & 0o777).toBe(0o600);
    chmodSync(file, 0o644);
    await expect(LocalKek.open(file)).rejects.toThrow(LocalKekError);
    await expect(LocalKek.open(file)).rejects.toThrow(/chmod 600/);
    chmodSync(file, 0o640);
    await expect(LocalKek.open(file)).rejects.toThrow(/chmod 600/);
    chmodSync(file, 0o600);
    await expect(LocalKek.open(file)).resolves.toBeInstanceOf(LocalKek);
  });

  it('errors name the file, never its contents', async () => {
    const file = join(dir(), 'kek.json');
    writeFileSync(file, '{"not": "a key file", "fake": "c2VjcmV0LWtleS1ieXRlcw=="}', { mode: 0o600 });
    const error = (await LocalKek.open(file).catch((e: unknown) => e)) as Error;
    expect(error).toBeInstanceOf(LocalKekError);
    expect(error.message).toContain(file);
    expect(error.message).not.toContain('c2VjcmV0');
    await expect(LocalKek.open(join(dir(), 'missing.json'))).rejects.toThrow(LocalKekError);
  });

  it('never overwrites an existing key file', async () => {
    const file = join(dir(), 'kek.json');
    await generateLocalKeyFile(file);
    const before = readFileSync(file, 'utf8');
    await expect(generateLocalKeyFile(file)).rejects.toThrow();
    expect(readFileSync(file, 'utf8')).toBe(before);
  });

  it('unwrap needs the same context and key file', async () => {
    const file = join(dir(), 'kek.json');
    await generateLocalKeyFile(file);
    const kek = await LocalKek.open(file);
    const dek = Buffer.alloc(32, 7);
    const { wrapped, kekRef } = await kek.wrap(dek, ctx);
    expect(kekRef).toMatch(/^local\/[0-9a-f]{16}\/1$/);
    expect((await kek.unwrap(wrapped, kekRef, ctx)).equals(dek)).toBe(true);
    await expect(kek.unwrap(wrapped, kekRef, { ...ctx, credential_id: 'cr_2' })).rejects.toBeInstanceOf(DecryptError);
    const otherFile = join(dir(), 'kek.json');
    await generateLocalKeyFile(otherFile);
    const other = await LocalKek.open(otherFile);
    await expect(other.unwrap(wrapped, kekRef, ctx)).rejects.toBeInstanceOf(DecryptError);
  });

  it('rotation adds a version and keeps the old ones', async () => {
    const file = join(dir(), 'kek.json');
    await generateLocalKeyFile(file);
    const kek = await LocalKek.open(file);
    const old = await kek.wrap(Buffer.alloc(32, 1), ctx);
    await kek.rotate();
    const fresh = await kek.wrap(Buffer.alloc(32, 1), ctx);
    expect(fresh.kekRef.endsWith('/2')).toBe(true);
    expect(kek.describe()).toMatchObject({ provider: 'local', version: '2' });
    expect(kek.describe().warning).toMatch(/anyone with this host can decrypt/);
    expect((await kek.unwrap(old.wrapped, old.kekRef, ctx)).equals(Buffer.alloc(32, 1))).toBe(true);
    expect(statSync(file).mode & 0o777).toBe(0o600);
  });
});
