-- R0-1 (runner design §7.5, §11.4): Ed25519 signing keys, one active key
-- per purpose ('grant' for release grants, 'user' for user assertions).
-- The private key is sealed with the server's secret key (AES-256-GCM,
-- the webhook sealer) and never stored in the clear. A rotated key keeps
-- its row with retired_at_ms set and stays in the JWKS for 24 hours.
CREATE TABLE signing_keys (
    kid TEXT NOT NULL PRIMARY KEY,
    purpose TEXT NOT NULL CHECK (purpose IN ('grant', 'user')),
    sealed_private TEXT NOT NULL,
    public BLOB NOT NULL,
    created_at_ms INTEGER NOT NULL,
    retired_at_ms INTEGER
);

CREATE UNIQUE INDEX signing_keys_active
    ON signing_keys (purpose)
    WHERE retired_at_ms IS NULL;
