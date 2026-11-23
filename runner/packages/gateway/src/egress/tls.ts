import { generateKeyPair, randomBytes } from 'node:crypto';
import { promisify } from 'node:util';
import { createSecureContext } from 'node:tls';
import forge from 'node-forge';

const keys = promisify(generateKeyPair);
async function rsa() {
  return keys('rsa', {
    modulusLength: 2048,
    publicKeyEncoding: { type: 'spki', format: 'pem' },
    privateKeyEncoding: { type: 'pkcs8', format: 'pem' },
  });
}

function certificate(publicKey: string, name: string, expires: number) {
  const cert = forge.pki.createCertificate();
  cert.publicKey = forge.pki.publicKeyFromPem(publicKey);
  cert.serialNumber = '01' + randomBytes(16).toString('hex');
  cert.validity.notBefore = new Date(Date.now() - 60_000);
  cert.validity.notAfter = new Date(expires);
  cert.setSubject([{ name: 'commonName', value: name }]);
  return cert;
}

export async function sandboxCA() {
  const [rootKeys, leafKeys] = await Promise.all([rsa(), rsa()]);
  // Lease renewal retains this trust anchor. Leaf certificates are lease-bound.
  const root = certificate(rootKeys.publicKey, 'Runbook sandbox', Date.now() + 365 * 86_400_000);
  const rootKey = forge.pki.privateKeyFromPem(rootKeys.privateKey);
  root.setIssuer(root.subject.attributes);
  root.setExtensions([{ name: 'basicConstraints', cA: true, pathLenConstraint: 0 }, { name: 'keyUsage', keyCertSign: true }]);
  root.sign(rootKey, forge.md.sha256.create());
  return {
    pem: forge.pki.certificateToPem(root),
    context(host: string, expires: number) {
      const leaf = certificate(leafKeys.publicKey, host, expires);
      leaf.setIssuer(root.subject.attributes);
      leaf.setExtensions([
        { name: 'basicConstraints', cA: false },
        { name: 'extKeyUsage', serverAuth: true },
        { name: 'subjectAltName', altNames: [{ type: 2, value: host }] },
      ]);
      leaf.sign(rootKey, forge.md.sha256.create());
      return createSecureContext({ key: leafKeys.privateKey, cert: forge.pki.certificateToPem(leaf) });
    },
  };
}
