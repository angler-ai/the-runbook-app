import type pg from 'pg';
import { AssertionVerifier as Verifier, type AssertionOptions } from '@runbook/runner-shared/user-assertion';

export class AssertionVerifier extends Verifier {
  constructor({ db, ...options }: Omit<AssertionOptions, 'consumeReplay'> & { db: pg.Pool }) {
    super({
      ...options,
      async consumeReplay(install, jti, expiresAt) {
        const inserted = await db.query(
          `INSERT INTO assertion_replays (install_id, jti, expires_at) VALUES ($1, $2, $3)
           ON CONFLICT (install_id, jti) DO NOTHING`, [install, jti, expiresAt],
        );
        return inserted.rowCount === 1;
      },
    });
  }
}
