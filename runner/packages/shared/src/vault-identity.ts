import { createHash, createPrivateKey, createPublicKey, hkdfSync, type KeyObject } from 'node:crypto';

const PKCS8_ED25519_SEED = Buffer.from('302e020100300506032b657004220420', 'hex');

export function composeIdentitySeed(initialKek: Buffer): Buffer {
  return Buffer.from(hkdfSync('sha256', initialKek, '', 'runbook-runner/compose-vault-identity/v1', 32));
}

export function vaultIdentityPrivateKey(seed: Buffer): KeyObject {
  const der = Buffer.concat([PKCS8_ED25519_SEED, seed]);
  try {
    return createPrivateKey({ key: der, format: 'der', type: 'pkcs8' });
  } finally {
    der.fill(0);
  }
}

export function vaultIdentityPublicKey(privateKey: KeyObject): string {
  const { x } = createPublicKey(privateKey).export({ format: 'jwk' });
  if (!x) throw new Error('vault identity has no public key');
  return x;
}

export function vaultIdentityFingerprint(publicKey: string): string {
  return createHash('sha256').update(Buffer.from(publicKey, 'base64url')).digest('hex');
}
