import { defineRequestState, hasRequestState, tryGetCurrentAuthEndpointContext } from '@better-auth/core/context';
import { APIError } from 'better-auth/api';
import type { DBAdapter, DBAdapterInstance } from 'better-auth';

const generation = defineRequestState<number | undefined>(() => undefined);

export async function bindMfaGeneration(expected: number) {
  if (await generation.get() === undefined) await generation.set(expected);
}

export async function boundMfaGeneration(): Promise<number | undefined> {
  return await hasRequestState() ? generation.get() : undefined;
}

function generationConflict(error: unknown): boolean {
  if (!(error instanceof Error)) return false;
  return error.message.includes('MFA generation changed') || generationConflict(error.cause);
}

export function guardedAuthAdapter(factory: DBAdapterInstance): DBAdapterInstance {
  return (options) => {
    const adapter = factory(options);
    const create: DBAdapter['create'] = async (query) => {
      if (query.model !== 'passkey' && query.model !== 'session') return adapter.create(query);
      const setupChanged = () => new APIError('FORBIDDEN', {
        code: 'auth.mfa_setup_changed', message: 'Start passkey setup again.',
      });
      const expected = await boundMfaGeneration();
      if (query.model === 'passkey' && expected === undefined) throw setupChanged();
      try {
        return await adapter.create(query.model === 'passkey'
          ? { ...query, data: { ...query.data, mfaGeneration: expected } }
          : query);
      } catch (error) {
        if (!generationConflict(error)) throw error;
        if (query.model === 'passkey') throw setupChanged();
        const assertion = tryGetCurrentAuthEndpointContext()?.path === '/passkey/verify-authentication';
        if (assertion) throw new APIError('UNAUTHORIZED', {
          code: 'auth.passkey_assertion_failed', message: 'Could not verify this passkey.',
        });
        throw new APIError('UNAUTHORIZED', { code: 'auth.sign_in_again', message: 'Sign in again.' });
      }
    };
    const findOne: DBAdapter['findOne'] = async <T>(query: Parameters<DBAdapter['findOne']>[0]) => {
      const result = await adapter.findOne<T>(query);
      const ceremony = tryGetCurrentAuthEndpointContext();
      if (ceremony?.path === '/passkey/verify-authentication' && query.model === 'passkey') {
        const factor = result as { mfaGeneration?: number } | null;
        if (factor) await bindMfaGeneration(factor.mfaGeneration ?? 0);
      }
      return result;
    };
    return { ...adapter, create, findOne };
  };
}
