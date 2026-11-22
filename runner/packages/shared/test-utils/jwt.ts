// Signing for test fakes only: the runner never signs core tokens.
import { generateKeyPairSync, randomBytes, sign, type KeyObject } from 'node:crypto';
import type { Jwk } from '../src/contracts/core.ts';

export interface SigningKey {
  kid: string;
  privateKey: KeyObject;
  jwk: Jwk;
}

export function generateSigningKey(kid = `grant-${randomBytes(4).toString('hex')}`): SigningKey {
  const { privateKey, publicKey } = generateKeyPairSync('ed25519');
  const x = (publicKey.export({ format: 'jwk' }) as { x: string }).x;
  return { kid, privateKey, jwk: { kty: 'OKP', crv: 'Ed25519', x, kid, use: 'sig', alg: 'EdDSA' } };
}

export function signJwt(key: SigningKey, typ: string, claims: object, alg = 'EdDSA'): string {
  const header = Buffer.from(JSON.stringify({ alg, typ, kid: key.kid })).toString('base64url');
  const body = Buffer.from(JSON.stringify(claims)).toString('base64url');
  const signature = sign(null, Buffer.from(`${header}.${body}`), key.privateKey).toString('base64url');
  return `${header}.${body}.${signature}`;
}
