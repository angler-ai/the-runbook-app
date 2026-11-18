import { describe, expect, it } from 'vitest';
import { createQueryClient } from '../App';
import { ApiError } from './client';
import { retryQuery } from './query-policy';

const apiError = (status: number) => new ApiError({ status, code: `http.${status}` });

describe('query retry policy (U15)', () => {
  it('never retries a definitive 4xx', () => {
    for (const status of [400, 401, 403, 404, 409, 410, 412, 422]) {
      expect(retryQuery(0, apiError(status))).toBe(false);
    }
  });

  it('retries a transient failure once', () => {
    for (const error of [
      apiError(500),
      apiError(503),
      apiError(408),
      apiError(429),
      new TypeError('Failed to fetch'),
    ]) {
      expect(retryQuery(0, error)).toBe(true);
      expect(retryQuery(1, error)).toBe(false);
    }
  });

  it('is the app query client default', () => {
    expect(createQueryClient().getDefaultOptions().queries?.retry).toBe(retryQuery);
  });
});
