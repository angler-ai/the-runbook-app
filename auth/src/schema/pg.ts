// Better Auth's tables on Postgres (server.ts), in their own `auth` schema. Rust
// never reads or writes this schema (D43, B2.1); the Rust side learns about
// sessions only through POST /internal/session. Same shape as ./sqlite.ts.
import { relations } from 'drizzle-orm';
import { bigint, boolean, index, integer, jsonb, pgSchema, text, timestamp, uniqueIndex } from 'drizzle-orm/pg-core';

export const authSchema = pgSchema('auth');

const at = (name: string) => timestamp(name, { withTimezone: true, mode: 'date' });

export const user = authSchema.table('user', {
  id: text('id').primaryKey(),
  name: text('name').notNull(),
  email: text('email').notNull().unique(),
  emailVerified: boolean('email_verified').default(false).notNull(),
  image: text('image'),
  twoFactorEnabled: boolean('two_factor_enabled').default(false).notNull(),
  mfaGeneration: integer('mfa_generation').default(0).notNull(),
  createdAt: at('created_at').defaultNow().notNull(),
  updatedAt: at('updated_at').defaultNow().$onUpdate(() => new Date()).notNull(),
});

export const session = authSchema.table(
  'session',
  {
    id: text('id').primaryKey(),
    expiresAt: at('expires_at').notNull(),
    token: text('token').notNull().unique(),
    createdAt: at('created_at').defaultNow().notNull(),
    updatedAt: at('updated_at').$onUpdate(() => new Date()).notNull(),
    ipAddress: text('ip_address'),
    userAgent: text('user_agent'),
    mfaVerifiedAt: at('mfa_verified_at'),
    mfaMethod: text('mfa_method'),
    mfaGeneration: integer('mfa_generation').default(-1).notNull(),
    userId: text('user_id').notNull().references(() => user.id, { onDelete: 'cascade' }),
  },
  (table) => [index('session_user_id_idx').on(table.userId)],
);

export const account = authSchema.table(
  'account',
  {
    id: text('id').primaryKey(),
    accountId: text('account_id').notNull(),
    providerId: text('provider_id').notNull(),
    userId: text('user_id').notNull().references(() => user.id, { onDelete: 'cascade' }),
    accessToken: text('access_token'),
    refreshToken: text('refresh_token'),
    idToken: text('id_token'),
    accessTokenExpiresAt: at('access_token_expires_at'),
    refreshTokenExpiresAt: at('refresh_token_expires_at'),
    scope: text('scope'),
    password: text('password'),
    createdAt: at('created_at').defaultNow().notNull(),
    updatedAt: at('updated_at').$onUpdate(() => new Date()).notNull(),
  },
  (table) => [index('account_user_id_idx').on(table.userId)],
);

export const verification = authSchema.table(
  'verification',
  {
    id: text('id').primaryKey(),
    identifier: text('identifier').notNull(),
    value: text('value').notNull(),
    expiresAt: at('expires_at').notNull(),
    createdAt: at('created_at').defaultNow().notNull(),
    updatedAt: at('updated_at').defaultNow().$onUpdate(() => new Date()).notNull(),
  },
  (table) => [index('verification_identifier_idx').on(table.identifier)],
);

export const rateLimit = authSchema.table('rate_limit', {
  id: text('id').primaryKey(),
  key: text('key').notNull().unique(),
  count: integer('count').notNull(),
  lastRequest: bigint('last_request', { mode: 'number' }).notNull(),
});

/** Ours, not Better Auth's: one row per hashed email or IP (src/limits.ts). */
export const sendLimit = authSchema.table('send_limit', {
  key: text('key').primaryKey(),
  windowStart: bigint('window_start', { mode: 'number' }).notNull(),
  count: integer('count').notNull(),
  lastSent: bigint('last_sent', { mode: 'number' }).notNull(),
});

// MCP2: the OAuth 2.1 provider's tables (@better-auth/oauth-provider, opaque
// tokens, no JWT plugin), as the Drizzle adapter's schema generator writes them
// for the plugin options in src/oauth.ts. Postgres keeps `string[]` as text[].
const list = (name: string) => text(name).array();

export const oauthClient = authSchema.table(
  'oauth_client',
  {
    id: text('id').primaryKey(),
    clientId: text('client_id').notNull().unique(),
    clientSecret: text('client_secret'),
    clientDiscoveryId: text('client_discovery_id'),
    disabled: boolean('disabled').default(false),
    skipConsent: boolean('skip_consent'),
    enableEndSession: boolean('enable_end_session'),
    subjectType: text('subject_type'),
    scopes: list('scopes'),
    clientCredentialsScopes: list('client_credentials_scopes').default([]),
    userId: text('user_id').references(() => user.id, { onDelete: 'cascade' }),
    createdAt: at('created_at'),
    updatedAt: at('updated_at'),
    name: text('name'),
    uri: text('uri'),
    icon: text('icon'),
    contacts: list('contacts'),
    tos: text('tos'),
    policy: text('policy'),
    softwareId: text('software_id'),
    softwareVersion: text('software_version'),
    softwareStatement: text('software_statement'),
    redirectUris: list('redirect_uris').notNull(),
    postLogoutRedirectUris: list('post_logout_redirect_uris'),
    backchannelLogoutUri: text('backchannel_logout_uri'),
    backchannelLogoutSessionRequired: boolean('backchannel_logout_session_required'),
    tokenEndpointAuthMethod: text('token_endpoint_auth_method'),
    applicationType: text('application_type'),
    jwks: text('jwks'),
    jwksUri: text('jwks_uri'),
    grantTypes: list('grant_types'),
    responseTypes: list('response_types'),
    requirePKCE: boolean('require_pkce'),
    dpopBoundAccessTokens: boolean('dpop_bound_access_tokens').default(false),
    referenceId: text('reference_id'),
    metadata: jsonb('metadata'),
  },
  (table) => [index('oauth_client_user_id_idx').on(table.userId)],
);

export const oauthResource = authSchema.table('oauth_resource', {
  id: text('id').primaryKey(),
  identifier: text('identifier').notNull().unique(),
  name: text('name').notNull(),
  accessTokenTtl: integer('access_token_ttl'),
  refreshTokenTtl: integer('refresh_token_ttl'),
  signingAlgorithm: text('signing_algorithm'),
  signingKeyId: text('signing_key_id'),
  allowedScopes: list('allowed_scopes'),
  customClaims: jsonb('custom_claims'),
  dpopBoundAccessTokensRequired: boolean('dpop_bound_access_tokens_required').default(false),
  disabled: boolean('disabled').default(false),
  createdAt: at('created_at'),
  updatedAt: at('updated_at'),
  policyVersion: integer('policy_version').default(1),
  metadata: jsonb('metadata'),
});

export const oauthClientResource = authSchema.table(
  'oauth_client_resource',
  {
    id: text('id').primaryKey(),
    clientId: text('client_id').notNull().references(() => oauthClient.clientId, { onDelete: 'cascade' }),
    resourceId: text('resource_id').notNull().references(() => oauthResource.identifier, { onDelete: 'cascade' }),
    metadata: jsonb('metadata'),
    createdAt: at('created_at'),
  },
  (table) => [
    uniqueIndex('oauth_client_resource_client_resource_uidx').on(table.clientId, table.resourceId),
    index('oauth_client_resource_client_id_idx').on(table.clientId),
    index('oauth_client_resource_resource_id_idx').on(table.resourceId),
  ],
);

export const oauthRefreshToken = authSchema.table(
  'oauth_refresh_token',
  {
    id: text('id').primaryKey(),
    mfaVerifiedAt: at('mfa_verified_at'),
    mfaMethod: text('mfa_method'),
    token: text('token').notNull().unique(),
    clientId: text('client_id').notNull().references(() => oauthClient.clientId, { onDelete: 'cascade' }),
    sessionId: text('session_id').references(() => session.id, { onDelete: 'set null' }),
    userId: text('user_id').notNull().references(() => user.id, { onDelete: 'cascade' }),
    referenceId: text('reference_id'),
    authorizationCodeId: text('authorization_code_id'),
    resources: list('resources'),
    requestedUserInfoClaims: list('requested_user_info_claims'),
    expiresAt: at('expires_at'),
    createdAt: at('created_at'),
    revoked: at('revoked'),
    rotatedAt: at('rotated_at'),
    rotationReplayResponse: text('rotation_replay_response'),
    rotationReplayExpiresAt: at('rotation_replay_expires_at'),
    authTime: at('auth_time'),
    confirmation: jsonb('confirmation'),
    scopes: list('scopes').notNull(),
  },
  (table) => [
    index('oauth_refresh_token_client_id_idx').on(table.clientId),
    index('oauth_refresh_token_session_id_idx').on(table.sessionId),
    index('oauth_refresh_token_user_id_idx').on(table.userId),
    index('oauth_refresh_token_authorization_code_id_idx').on(table.authorizationCodeId),
  ],
);

export const oauthAccessToken = authSchema.table(
  'oauth_access_token',
  {
    id: text('id').primaryKey(),
    mfaVerifiedAt: at('mfa_verified_at'),
    mfaMethod: text('mfa_method'),
    token: text('token').unique(),
    clientId: text('client_id').notNull().references(() => oauthClient.clientId, { onDelete: 'cascade' }),
    sessionId: text('session_id').references(() => session.id, { onDelete: 'set null' }),
    userId: text('user_id').references(() => user.id, { onDelete: 'cascade' }),
    referenceId: text('reference_id'),
    authorizationCodeId: text('authorization_code_id'),
    resources: list('resources'),
    requestedUserInfoClaims: list('requested_user_info_claims'),
    refreshId: text('refresh_id').references(() => oauthRefreshToken.id, { onDelete: 'cascade' }),
    expiresAt: at('expires_at'),
    createdAt: at('created_at'),
    revoked: at('revoked'),
    confirmation: jsonb('confirmation'),
    scopes: list('scopes').notNull(),
  },
  (table) => [
    index('oauth_access_token_client_id_idx').on(table.clientId),
    index('oauth_access_token_session_id_idx').on(table.sessionId),
    index('oauth_access_token_user_id_idx').on(table.userId),
    index('oauth_access_token_authorization_code_id_idx').on(table.authorizationCodeId),
    index('oauth_access_token_refresh_id_idx').on(table.refreshId),
  ],
);

export const oauthConsent = authSchema.table(
  'oauth_consent',
  {
    id: text('id').primaryKey(),
    clientId: text('client_id').notNull().references(() => oauthClient.clientId, { onDelete: 'cascade' }),
    userId: text('user_id').references(() => user.id, { onDelete: 'cascade' }),
    referenceId: text('reference_id'),
    resources: list('resources'),
    requestedUserInfoClaims: list('requested_user_info_claims'),
    scopes: list('scopes').notNull(),
    createdAt: at('created_at'),
    updatedAt: at('updated_at'),
  },
  (table) => [index('oauth_consent_client_id_idx').on(table.clientId), index('oauth_consent_user_id_idx').on(table.userId)],
);

export const oauthClientAssertion = authSchema.table('oauth_client_assertion', {
  id: text('id').primaryKey(),
  expiresAt: at('expires_at').notNull(),
});

export const userRelations = relations(user, ({ many }) => ({ sessions: many(session), accounts: many(account) }));
export const sessionRelations = relations(session, ({ one }) => ({ user: one(user, { fields: [session.userId], references: [user.id] }) }));
export const accountRelations = relations(account, ({ one }) => ({ user: one(user, { fields: [account.userId], references: [user.id] }) }));

export const twoFactor = authSchema.table('two_factor', {
  id: text('id').primaryKey(),
  secret: text('secret').notNull(),
  backupCodes: text('backup_codes').notNull(),
  userId: text('user_id').notNull().unique().references(() => user.id, { onDelete: 'cascade' }),
  verified: boolean('verified').default(true),
  failedVerificationCount: integer('failed_verification_count').default(0),
  lockedUntil: at('locked_until'),
});

export const passkey = authSchema.table('passkey', {
  id: text('id').primaryKey(),
  name: text('name'),
  publicKey: text('public_key').notNull(),
  userId: text('user_id').notNull().references(() => user.id, { onDelete: 'cascade' }),
  credentialID: text('credential_id').notNull().unique(),
  counter: integer('counter').notNull(),
  deviceType: text('device_type').notNull(),
  backedUp: boolean('backed_up').notNull(),
  transports: text('transports'),
  createdAt: at('created_at'),
  lastUsedAt: at('last_used_at'),
  mfaGeneration: integer('mfa_generation').default(-1).notNull(),
  aaguid: text('aaguid'),
}, (table) => [index('auth_passkey_user_id_idx').on(table.userId)]);

export const mfaAudit = authSchema.table('mfa_audit', {
  id: text('id').primaryKey(),
  userId: text('user_id').notNull(),
  action: text('action').notNull(),
  actor: text('actor').notNull(),
  occurredAt: at('occurred_at').notNull(),
  notification: text('notification'),
  notifyAttemptedAt: at('notify_attempted_at'),
  notifiedAt: at('notified_at'),
}, (table) => [index('auth_mfa_audit_user_idx').on(table.userId, table.occurredAt)]);
