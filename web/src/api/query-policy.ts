// React Query retry policy (U15). A query retries once after a transient
// failure (network error, 5xx, 408, 429). A definitive 4xx answer, such as
// 401, 403, 404, 410 or 422, would only come back the same, so it is shown
// at once instead of after a pointless second request.
import { ApiError } from './client';

const TRANSIENT_4XX = new Set([408, 429]);

export function isDefinitiveClientError(error: unknown): boolean {
  return (
    error instanceof ApiError &&
    error.status >= 400 &&
    error.status < 500 &&
    !TRANSIENT_4XX.has(error.status)
  );
}

export function retryQuery(failureCount: number, error: unknown): boolean {
  return failureCount < 1 && !isDefinitiveClientError(error);
}
