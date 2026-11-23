// Errors the vault answers with. Codes and messages are fixed strings: they
// never quote a request, a secret, a driver message or an upstream response.

export class VaultError extends Error {
  readonly status: number;
  readonly code: string;
  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = 'VaultError';
    this.status = status;
    this.code = code;
  }
}

export const refused = (status: number, code: string, message: string) => new VaultError(status, code, message);
