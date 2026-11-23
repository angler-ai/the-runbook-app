// Key-encryption key providers (design §7.3, §14): one per install. Only
// the vault holds one. `wrap`/`unwrap` protect data keys (and the vault's own
// keys) with a wrap context that names what the key belongs to; `kekRef`
// names the provider, key and key version used.
import type { KekConfig } from '@runbook/runner-shared/config';

/** What a wrapped key belongs to; bound into the wrap (AAD or KMS context). */
export type WrapContext = Record<string, string>;

export interface KekInfo {
  provider: 'local' | 'gcp-kms';
  key: string;
  version?: string;
  /** Shown in the UI (for example the local key's single-trust-domain warning). */
  warning?: string;
}

export interface Kek {
  bootstrapIdentitySeed?(): Buffer | undefined;
  wrap(dek: Buffer, ctx: WrapContext): Promise<{ wrapped: Buffer; kekRef: string }>;
  unwrap(wrapped: Buffer, kekRef: string, ctx: WrapContext): Promise<Buffer>;
  describe(): KekInfo;
}

export async function createKek(config: KekConfig): Promise<Kek> {
  switch (config.provider) {
    case 'local': {
      const { LocalKek } = await import('./local.ts');
      return LocalKek.open(config.file);
    }
    case 'gcp-kms': {
      const { GcpKmsKek } = await import('./gcp-kms.ts');
      return new GcpKmsKek(config.key);
    }
  }
}
