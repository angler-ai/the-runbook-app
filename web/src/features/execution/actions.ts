// One user action, one idempotency key (D20, A5), reused on every retry of
// that action until its outcome is definitive. The action id names the
// intent (route, target and payload), so clicking "Rerun" again after a
// lost response reuses the key and the server replays the first result,
// while a different payload (another option, another signal) is a new
// action. Keys live in the app's API runtime (`idempotencyKeyFor` /
// `forgetAction`), so they survive re-renders and remounts in this tab.
import { ApiError, apiRuntime } from '../../api/client';

/** A stable id for one intent: the same scope and payload give the same id. */
export function actionId(scope: string, intent?: unknown): string {
  return intent === undefined ? scope : `${scope} ${JSON.stringify(intent)}`;
}

/**
 * A refusal that the same request would get again: the action is over.
 * Network failures, 5xx, 408, 425 and 429 leave the outcome unknown, so
 * the next attempt is a retry with the same key.
 */
export function isDefinitive(error: unknown): boolean {
  return (
    error instanceof ApiError &&
    error.status >= 400 &&
    error.status < 500 &&
    error.status !== 408 &&
    error.status !== 425 &&
    error.status !== 429
  );
}

/** Actions whose last attempt ended without a definitive outcome. */
const uncertain = new Set<string>();

export interface RunActionOptions {
  /**
   * For routes without replay (PATCH, D27): a refusal that, on a retry of
   * an attempt whose response was lost, means the first attempt landed.
   */
  landedEarlier?(error: ApiError): boolean;
}

/**
 * One attempt of an action. `send` gets the action's idempotency key.
 * Success or a definitive refusal forgets the key (the next click is a new
 * action); anything else keeps it for the retry.
 */
export function runAction<T>(id: string, send: (idempotencyKey: string) => Promise<T>): Promise<T>;
export function runAction<T>(
  id: string,
  send: (idempotencyKey: string) => Promise<T>,
  options: RunActionOptions,
): Promise<T | undefined>;
export async function runAction<T>(
  id: string,
  send: (idempotencyKey: string) => Promise<T>,
  options: RunActionOptions = {},
): Promise<T | undefined> {
  const retry = uncertain.has(id);
  try {
    const result = await send(apiRuntime.idempotencyKeyFor(id));
    uncertain.delete(id);
    apiRuntime.forgetAction(id);
    return result;
  } catch (error) {
    if (isDefinitive(error)) {
      uncertain.delete(id);
      apiRuntime.forgetAction(id);
      if (retry && error instanceof ApiError && options.landedEarlier?.(error)) return undefined;
    } else {
      uncertain.add(id);
    }
    throw error;
  }
}
