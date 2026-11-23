import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { beforeEach, describe, expect, it } from 'vitest';
import { type AadFields, encodeAad, lengthPrefixed } from '../src/crypto/aad.ts';
import { bindingHash } from '../src/crypto/binding.ts';
import { BindingMismatch, DecryptError, open, rewrap, seal } from '../src/crypto/envelope.ts';
import { fingerprint } from '../src/crypto/fingerprint.ts';
import type { Kek, WrapContext } from '../src/kek/index.ts';
import { generateLocalKeyFile, LocalKek } from '../src/kek/local.ts';

const SECRET = Buffer.from('fake-api-key-for-tests-0001');
const binding = { hosts: ['api.datadoghq.com'], header_name: 'dd-api-key', header_format: '{}', groups: 'all' };

function fields(overrides: Partial<AadFields> = {}): AadFields {
  return {
    orgId: 'org_1',
    credentialId: 'cr_1',
    field: 'key',
    version: 1,
    type: 'api-key',
    bindingHash: bindingHash('api-key', binding),
    ...overrides,
  };
}
const ctx = (f: AadFields): WrapContext => ({ org_id: f.orgId, credential_id: f.credentialId });

/** A KEK that records the data keys it was handed (by reference). */
class RecordingKek implements Kek {
  readonly seen: Buffer[] = [];
  private readonly inner: Kek;
  constructor(inner: Kek) {
    this.inner = inner;
  }
  async wrap(dek: Buffer, c: WrapContext) {
    this.seen.push(dek);
    return this.inner.wrap(dek, c);
  }
  unwrap(wrapped: Buffer, kekRef: string, c: WrapContext) {
    return this.inner.unwrap(wrapped, kekRef, c);
  }
  describe() {
    return this.inner.describe();
  }
}

describe('envelope encryption', () => {
  let kek: LocalKek;
  let keyFile: string;
  beforeEach(async () => {
    keyFile = join(mkdtempSync(join(tmpdir(), 'rb-kek-')), 'kek.json');
    await generateLocalKeyFile(keyFile);
    kek = await LocalKek.open(keyFile);
  });

  it('round_trip', async () => {
    const f = fields();
    const row = await seal(kek, SECRET, f, ctx(f));
    expect(row.ciphertext.includes(SECRET)).toBe(false);
    expect(row.bindingHash.equals(f.bindingHash)).toBe(true);
    expect(row.nonce.length).toBe(12);
    expect((await open(kek, row, f)).equals(SECRET)).toBe(true);
  });

  it('fresh_dek_and_nonce_per_seal', async () => {
    const recording = new RecordingKek(kek);
    const f = fields();
    const a = await seal(recording, SECRET, f, ctx(f));
    const b = await seal(recording, SECRET, f, ctx(f));
    expect(a.nonce.equals(b.nonce)).toBe(false);
    expect(a.wrappedDek.equals(b.wrappedDek)).toBe(false);
    expect(a.ciphertext.equals(b.ciphertext)).toBe(false);
    expect(recording.seen).toHaveLength(2);
    // 256-bit keys, zeroed once wrapped and used.
    for (const dek of recording.seen) {
      expect(dek.length).toBe(32);
      expect(dek.every((byte) => byte === 0)).toBe(true);
    }
  });

  it('changed_binding_fails_open', async () => {
    const f = fields();
    const row = await seal(kek, SECRET, f, ctx(f));
    const moved = fields({ bindingHash: bindingHash('api-key', { ...binding, hosts: ['evil.example.com'] }) });
    await expect(open(kek, row, moved)).rejects.toBeInstanceOf(BindingMismatch);
    // Even if the stored hash were rewritten to match, the AAD still fails.
    await expect(open(kek, { ...row, bindingHash: moved.bindingHash }, moved)).rejects.toBeInstanceOf(DecryptError);
  });

  it('row_copied_to_other_credential_fails', async () => {
    const f = fields();
    const row = await seal(kek, SECRET, f, ctx(f));
    await expect(open(kek, row, fields({ credentialId: 'cr_2' }))).rejects.toBeInstanceOf(DecryptError);
    await expect(open(kek, row, fields({ field: 'other' }))).rejects.toBeInstanceOf(DecryptError);
    await expect(open(kek, row, fields({ version: 2 }))).rejects.toBeInstanceOf(DecryptError);
    await expect(open(kek, row, fields({ orgId: 'org_2' }))).rejects.toBeInstanceOf(DecryptError);
    await expect(open(kek, row, fields({ type: 'mcp-bearer' }))).rejects.toBeInstanceOf(DecryptError);
  });

  it('aad_encoding_is_unambiguous', () => {
    expect(lengthPrefixed(['ab', 'c']).equals(lengthPrefixed(['a', 'bc']))).toBe(false);
    expect(lengthPrefixed(['', 'abc']).equals(lengthPrefixed(['abc', '']))).toBe(false);
    const a = encodeAad(fields({ credentialId: 'cr_1', field: 'xkey' }));
    const b = encodeAad(fields({ credentialId: 'cr_1x', field: 'key' }));
    expect(a.equals(b)).toBe(false);
    // 4-byte big-endian length before each field.
    expect(lengthPrefixed(['ab']).equals(Buffer.from([0, 0, 0, 2, 0x61, 0x62]))).toBe(true);
    // The KEK version is not in the data AAD.
    expect(encodeAad(fields()).toString('latin1')).not.toContain('local/');
  });

  it('rewrap_keeps_ciphertext', async () => {
    const f = fields();
    const row = await seal(kek, SECRET, f, ctx(f));
    await kek.rotate();
    const rotated = await rewrap(kek, row, ctx(f));
    expect(rotated.kekRef).not.toBe(row.kekRef);
    expect(rotated.wrappedDek.equals(row.wrappedDek)).toBe(false);
    expect(rotated.ciphertext.equals(row.ciphertext)).toBe(true);
    expect(rotated.nonce.equals(row.nonce)).toBe(true);
    expect((await open(kek, rotated, f)).equals(SECRET)).toBe(true);
    // A fresh process reading the rotated file opens both.
    const reopened = await LocalKek.open(keyFile);
    expect((await open(reopened, row, f)).equals(SECRET)).toBe(true);
    expect((await open(reopened, rotated, f)).equals(SECRET)).toBe(true);
  });

  it('binding hash is canonical: key order does not matter, values do', () => {
    const a = bindingHash('api-key', { a: 1, b: ['x', 'y'] });
    expect(a.equals(bindingHash('api-key', { b: ['x', 'y'], a: 1 }))).toBe(true);
    expect(a.equals(bindingHash('api-key', { a: 1, b: ['y', 'x'] }))).toBe(false);
    expect(a.equals(bindingHash('mcp-bearer', { a: 1, b: ['x', 'y'] }))).toBe(false);
    expect(a.length).toBe(32);
  });

  it('fingerprint is a keyed HMAC, 8 hex characters', () => {
    const k1 = Buffer.alloc(32, 1);
    const k2 = Buffer.alloc(32, 2);
    expect(fingerprint(k1, SECRET)).toMatch(/^[0-9a-f]{8}$/);
    expect(fingerprint(k1, SECRET)).toBe(fingerprint(k1, SECRET));
    expect(fingerprint(k1, SECRET)).not.toBe(fingerprint(k2, SECRET));
  });
});
