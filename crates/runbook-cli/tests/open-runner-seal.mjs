import assert from 'node:assert/strict';
import { handleSeal } from '../../../runner/packages/vault/src/routes/seal.ts';
import {
  parseSealedBlob, sealAad, SEAL_INFO, sealSuite,
} from '../../../runner/packages/shared/src/seal-client.ts';
import { fromBase64Url } from '../../../runner/packages/shared/src/encoding.ts';

// RFC 7748 Alice's test key pair. The credential binding is asserted by the
// Rust test, and here the real vault handler must open that exact envelope.
const blob = process.argv[2];
const envelope = parseSealedBlob(blob);
const suite = sealSuite();
const recipientKey = await suite.kem.deserializePrivateKey(
  Buffer.from('77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a', 'hex'),
);
await assert.rejects(handleSeal({
  now: () => new Date(),
  orgId: 'other-org',
  ingest: { privateKey: async () => recipientKey },
}, { blob }, { kind: 'user', id: 'fixture' }), { message: 'the blob was sealed for another org' });

const recipient = await suite.createRecipientContext({
  recipientKey, enc: fromBase64Url(envelope.enc), info: SEAL_INFO,
});
const plaintext = await recipient.open(fromBase64Url(envelope.ct), sealAad(envelope));
assert.equal(new TextDecoder().decode(plaintext), 'test secret café');
