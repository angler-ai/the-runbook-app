// WebAuthn JSON <-> browser objects. The auth service speaks the WebAuthn
// JSON forms (base64url binary fields); navigator.credentials wants
// ArrayBuffers. Written out here rather than relying on the newer
// PublicKeyCredential.parse*FromJSON / toJSON, which not every supported
// browser ships.

type Json = Record<string, unknown>;

interface DescriptorJSON {
  id: string;
  type: 'public-key';
  transports?: AuthenticatorTransport[];
}

export interface CreationOptionsJSON extends Json {
  challenge: string;
  rp: { id?: string; name: string };
  user: { id: string; name: string; displayName: string };
  pubKeyCredParams: PublicKeyCredentialParameters[];
  excludeCredentials?: DescriptorJSON[];
  authenticatorSelection?: AuthenticatorSelectionCriteria;
  timeout?: number;
  attestation?: AttestationConveyancePreference;
}

export interface RequestOptionsJSON extends Json {
  challenge: string;
  rpId?: string;
  allowCredentials?: DescriptorJSON[];
  userVerification?: UserVerificationRequirement;
  timeout?: number;
}

export function base64urlToBuffer(value: string): ArrayBuffer {
  const base64 = value.replace(/-/g, '+').replace(/_/g, '/');
  const padded = base64 + '='.repeat((4 - (base64.length % 4)) % 4);
  const binary = atob(padded);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return bytes.buffer;
}

export function bufferToBase64url(value: ArrayBuffer | ArrayBufferView): string {
  // ArrayBuffer.isView, not instanceof: buffers can come from another realm.
  const bytes = ArrayBuffer.isView(value)
    ? new Uint8Array(value.buffer, value.byteOffset, value.byteLength)
    : new Uint8Array(value);
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

const descriptor = (d: DescriptorJSON): PublicKeyCredentialDescriptor => ({
  type: d.type,
  id: base64urlToBuffer(d.id),
  ...(d.transports ? { transports: d.transports } : {}),
});

export function toCreationOptions(json: CreationOptionsJSON): PublicKeyCredentialCreationOptions {
  return {
    challenge: base64urlToBuffer(json.challenge),
    rp: json.rp,
    user: { ...json.user, id: base64urlToBuffer(json.user.id) },
    pubKeyCredParams: json.pubKeyCredParams,
    excludeCredentials: (json.excludeCredentials ?? []).map(descriptor),
    // The service already asks for required user verification; insist here too.
    authenticatorSelection: { ...json.authenticatorSelection, userVerification: 'required' },
    ...(json.timeout ? { timeout: json.timeout } : {}),
    ...(json.attestation ? { attestation: json.attestation } : {}),
  };
}

export function toRequestOptions(json: RequestOptionsJSON): PublicKeyCredentialRequestOptions {
  return {
    challenge: base64urlToBuffer(json.challenge),
    ...(json.rpId ? { rpId: json.rpId } : {}),
    allowCredentials: (json.allowCredentials ?? []).map(descriptor),
    userVerification: 'required',
    ...(json.timeout ? { timeout: json.timeout } : {}),
  };
}

/** A created or asserted credential in the JSON form the service verifies. */
export function credentialToJSON(credential: PublicKeyCredential): Json {
  const response = credential.response;
  const out: Json = {
    id: credential.id,
    rawId: bufferToBase64url(credential.rawId),
    type: credential.type,
    clientExtensionResults: credential.getClientExtensionResults?.() ?? {},
    ...(credential.authenticatorAttachment
      ? { authenticatorAttachment: credential.authenticatorAttachment }
      : {}),
  };
  if ('attestationObject' in response) {
    const attestation = response as AuthenticatorAttestationResponse;
    out.response = {
      clientDataJSON: bufferToBase64url(attestation.clientDataJSON),
      attestationObject: bufferToBase64url(attestation.attestationObject),
      transports: attestation.getTransports?.() ?? [],
    };
  } else {
    const assertion = response as AuthenticatorAssertionResponse;
    out.response = {
      clientDataJSON: bufferToBase64url(assertion.clientDataJSON),
      authenticatorData: bufferToBase64url(assertion.authenticatorData),
      signature: bufferToBase64url(assertion.signature),
      ...(assertion.userHandle ? { userHandle: bufferToBase64url(assertion.userHandle) } : {}),
    };
  }
  return out;
}

/** Whether this browser can use passkeys at all. */
export function passkeysSupported(): boolean {
  return (
    typeof globalThis.PublicKeyCredential === 'function' &&
    typeof globalThis.navigator?.credentials?.create === 'function'
  );
}
