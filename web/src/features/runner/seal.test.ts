import { expect, test, vi } from 'vitest';
import { sealSuite, parseSealedBlob } from './seal-client';
import { fromBase64Url, lengthPrefixed, toBase64Url } from './encoding';
import { credentialBindingHash, destinationBinding } from './destination-binding';
import { saveCredentialSecrets } from './seal';
import vectors from '../../../../api/runner-binding-vectors.json';

test('credential_form_seals_before_send and the vault can open the blob', async () => {
  const suite = sealSuite();
  const key = await suite.kem.generateKeyPair();
  const publicKey = toBase64Url(new Uint8Array(await suite.kem.serializePublicKey(key.publicKey)));
  const identity = await crypto.subtle.generateKey('Ed25519', true, ['sign', 'verify']);
  const identityKey = toBase64Url(
    new Uint8Array(await crypto.subtle.exportKey('raw', identity.publicKey)),
  );
  const ingest = {
    ingest_key_id: 'key_1',
    public_key: publicKey,
    expires_at: '2099-01-01T00:00:00Z',
    org_id: 'org_1',
    install_id: 'ri_1',
  };
  const statement = lengthPrefixed([
    'runbook-runner/ingest-key/v1',
    ingest.org_id,
    ingest.install_id,
    ingest.ingest_key_id,
    ingest.public_key,
    ingest.expires_at,
  ]);
  const signature = await crypto.subtle.sign(
    'Ed25519',
    identity.privateKey,
    new Uint8Array(statement),
  );
  const signed = { ...ingest, identity_signature: toBase64Url(new Uint8Array(signature)) };
  const request = vi.fn(async () => signed);
  const fixture = vectors[4]!;
  const secret = 'private-value-only-for-this-test';
  const metadata = {
    id: 'cred_1',
    binding_hash: 'lying-runner-api-hash',
    secret_meta: {},
    type: fixture.type,
    config: fixture.config,
    groups: fixture.groups,
  };
  await saveCredentialSecrets(
    request,
    'org_1',
    metadata,
    { token: secret },
    { installId: 'ri_1', identityKey },
  );
  const substitute = vi.fn(async () => ({
    ...signed,
    public_key: 'hSDwCYkwp1R0i33ctD73Wg2_Og0mOBr066SpjqqbTmo',
  }));
  await expect(
    saveCredentialSecrets(
      substitute,
      'org_1',
      metadata,
      { token: secret },
      { installId: 'ri_1', identityKey },
    ),
  ).rejects.toThrow('vault identity');
  expect(substitute).toHaveBeenCalledTimes(1);
  expect(request.mock.calls[1]).toEqual([
    '/credentials/cred_1/secrets/token',
    'POST',
    { blob: expect.any(String) },
  ]);
  expect(JSON.stringify(request.mock.calls)).not.toContain(secret);
  const [, , body] = request.mock.calls[1] as unknown as [string, string, { blob: string }];
  const packet = parseSealedBlob(body.blob);
  expect(packet).toMatchObject({
    org_id: 'org_1',
    credential_id: 'cred_1',
    field: 'token',
    expected_version: 0,
    binding_hash: fixture.hash,
  });
  const recipient = await suite.createRecipientContext({
    recipientKey: key.privateKey,
    enc: fromBase64Url(packet.enc),
    info: new TextEncoder().encode('runbook-runner/seal/v1'),
  });
  const plaintext = await recipient.open(
    fromBase64Url(packet.ct),
    lengthPrefixed([
      'runbook-runner/seal/v1',
      'org_1',
      'cred_1',
      'token',
      '0',
      fixture.hash,
      'key_1',
      String(packet.expires_at),
    ]),
  );
  expect(new TextDecoder().decode(plaintext)).toBe(secret);
});

test.each(vectors)('matches the vault vectors for $type', async (fixture) => {
  expect(destinationBinding(fixture)).toEqual(fixture.binding);
  await expect(credentialBindingHash(fixture)).resolves.toBe(fixture.hash);
});

test('refuses a substituted ingest key before sending a sealed blob', async () => {
  const fixture = vectors[4]!;
  const request = vi.fn(async () => ({
    ingest_key_id: 'substituted',
    public_key: 'hSDwCYkwp1R0i33ctD73Wg2_Og0mOBr066SpjqqbTmo',
    expires_at: '2099-01-01T00:00:00Z',
  }));
  await expect(
    saveCredentialSecrets(
      request,
      'org_1',
      { ...fixture, id: 'cred_1', secret_meta: {} },
      { token: 'fixture' },
      { installId: 'ri_1', identityKey: 'invalid-anchor' },
    ),
  ).rejects.toThrow('vault identity');
  expect(request).toHaveBeenCalledTimes(1);
});

test('a signed receipt cannot be substituted for a different credential ID', async () => {
  const { verifyCredentialReceipt } = await import('./seal');
  const identity = await crypto.subtle.generateKey('Ed25519', true, ['sign', 'verify']);
  const anchor = toBase64Url(
    new Uint8Array(await crypto.subtle.exportKey('raw', identity.publicKey)),
  );
  const expected = {
    ...vectors[4]!,
    name: 'New credential',
    label: 'read',
    sharing: { projects: ['p'] },
  };
  const signed = lengthPrefixed([
    'runbook-runner/credential-create/v1',
    'org',
    'install',
    'new-id',
    expected.name,
    expected.hash,
    expected.label,
    JSON.stringify(expected.sharing.projects),
  ]);
  const signature = await crypto.subtle.sign(
    'Ed25519',
    identity.privateKey,
    new Uint8Array(signed),
  );
  const reply = { id: 'new-id', creation_receipt: toBase64Url(new Uint8Array(signature)) };
  await expect(
    verifyCredentialReceipt(reply, expected, 'org', 'install', anchor),
  ).resolves.toBeUndefined();
  await expect(
    verifyCredentialReceipt({ ...reply, id: 'other-id' }, expected, 'org', 'install', anchor),
  ).rejects.toThrow('vault credential receipt');
});
