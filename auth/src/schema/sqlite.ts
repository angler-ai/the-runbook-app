// Better Auth's tables on D1 (worker.ts). Generated from auth.config.ts with the
// Better Auth CLI (see README "Schema"), then given an `auth_` table prefix so they
// cannot collide with the Rust control-plane tables that share the D1
// database. Better Auth addresses them by the object keys (`user`, `session`, ...),
// so the prefix is invisible to it. `send_limit` is ours: the per-email and per-IP
// send-code limits (src/limits.ts).
import { relations, sql } from 'drizzle-orm';
import { index, integer, sqliteTable, text, uniqueIndex } from 'drizzle-orm/sqlite-core';

const now = sql`(cast(unixepoch('subsecond') * 1000 as integer))`;

export const user = sqliteTable('auth_user', {
  id: text('id').primaryKey(),
  name: text('name').notNull(),
  email: text('email').notNull().unique(),
  emailVerified: integer('email_verified', { mode: 'boolean' }).default(false).notNull(),
  image: text('image'),
  createdAt: integer('created_at', { mode: 'timestamp_ms' }).default(now).notNull(),
  updatedAt: integer('updated_at', { mode: 'timestamp_ms' }).default(now).$onUpdate(() => new Date()).notNull(),
});

export const session = sqliteTable(
  'auth_session',
  {
    id: text('id').primaryKey(),
    expiresAt: integer('expires_at', { mode: 'timestamp_ms' }).notNull(),
    token: text('token').notNull().unique(),
    createdAt: integer('created_at', { mode: 'timestamp_ms' }).default(now).notNull(),
    updatedAt: integer('updated_at', { mode: 'timestamp_ms' }).$onUpdate(() => new Date()).notNull(),
    ipAddress: text('ip_address'),
    userAgent: text('user_agent'),
    userId: text('user_id').notNull().references(() => user.id, { onDelete: 'cascade' }),
  },
  (table) => [index('auth_session_user_id_idx').on(table.userId)],
);

export const account = sqliteTable(
  'auth_account',
  {
    id: text('id').primaryKey(),
    accountId: text('account_id').notNull(),
    providerId: text('provider_id').notNull(),
    userId: text('user_id').notNull().references(() => user.id, { onDelete: 'cascade' }),
    accessToken: text('access_token'),
    refreshToken: text('refresh_token'),
    idToken: text('id_token'),
    accessTokenExpiresAt: integer('access_token_expires_at', { mode: 'timestamp_ms' }),
    refreshTokenExpiresAt: integer('refresh_token_expires_at', { mode: 'timestamp_ms' }),
    scope: text('scope'),
    password: text('password'),
    createdAt: integer('created_at', { mode: 'timestamp_ms' }).default(now).notNull(),
    updatedAt: integer('updated_at', { mode: 'timestamp_ms' }).$onUpdate(() => new Date()).notNull(),
  },
  (table) => [index('auth_account_user_id_idx').on(table.userId)],
);

export const verification = sqliteTable(
  'auth_verification',
  {
    id: text('id').primaryKey(),
    identifier: text('identifier').notNull(),
    value: text('value').notNull(),
    expiresAt: integer('expires_at', { mode: 'timestamp_ms' }).notNull(),
    createdAt: integer('created_at', { mode: 'timestamp_ms' }).default(now).notNull(),
    updatedAt: integer('updated_at', { mode: 'timestamp_ms' }).default(now).$onUpdate(() => new Date()).notNull(),
  },
  (table) => [index('auth_verification_identifier_idx').on(table.identifier)],
);

export const rateLimit = sqliteTable('auth_rate_limit', {
  id: text('id').primaryKey(),
  key: text('key').notNull().unique(),
  count: integer('count').notNull(),
  lastRequest: integer('last_request').notNull(),
});

/** Ours, not Better Auth's: one row per hashed email or IP (src/limits.ts). */
export const sendLimit = sqliteTable('auth_send_limit', {
  key: text('key').primaryKey(),
  windowStart: integer('window_start').notNull(),
  count: integer('count').notNull(),
  lastSent: integer('last_sent').notNull(),
});

// MCP2: the OAuth 2.1 provider's tables (@better-auth/oauth-provider, opaque
// tokens, no JWT plugin). Generated with the Drizzle adapter's schema generator
// for exactly the plugin options in src/oauth.ts, then given the `auth_` prefix.
// Better Auth stores `string[]` and `json` as JSON text on SQLite.
const json = (name: string) => text(name, { mode: 'json' });
const ms = (name: string) => integer(name, { mode: 'timestamp_ms' });

export const oauthClient = sqliteTable(
  'auth_oauth_client',
  {
    id: text('id').primaryKey(),
    clientId: text('client_id').notNull().unique(),
    clientSecret: text('client_secret'),
    clientDiscoveryId: text('client_discovery_id'),
    disabled: integer('disabled', { mode: 'boolean' }).default(false),
    skipConsent: integer('skip_consent', { mode: 'boolean' }),
    enableEndSession: integer('enable_end_session', { mode: 'boolean' }),
    subjectType: text('subject_type'),
    scopes: json('scopes'),
    clientCredentialsScopes: json('client_credentials_scopes').default([]),
    userId: text('user_id').references(() => user.id, { onDelete: 'cascade' }),
    createdAt: ms('created_at'),
    updatedAt: ms('updated_at'),
    name: text('name'),
    uri: text('uri'),
    icon: text('icon'),
    contacts: json('contacts'),
    tos: text('tos'),
    policy: text('policy'),
    softwareId: text('software_id'),
    softwareVersion: text('software_version'),
    softwareStatement: text('software_statement'),
    redirectUris: json('redirect_uris').notNull(),
    postLogoutRedirectUris: json('post_logout_redirect_uris'),
    backchannelLogoutUri: text('backchannel_logout_uri'),
    backchannelLogoutSessionRequired: integer('backchannel_logout_session_required', { mode: 'boolean' }),
    tokenEndpointAuthMethod: text('token_endpoint_auth_method'),
    applicationType: text('application_type'),
    jwks: text('jwks'),
    jwksUri: text('jwks_uri'),
    grantTypes: json('grant_types'),
    responseTypes: json('response_types'),
    requirePKCE: integer('require_pkce', { mode: 'boolean' }),
    dpopBoundAccessTokens: integer('dpop_bound_access_tokens', { mode: 'boolean' }).default(false),
    referenceId: text('reference_id'),
    metadata: json('metadata'),
  },
  (table) => [index('auth_oauth_client_user_id_idx').on(table.userId)],
);

export const oauthResource = sqliteTable('auth_oauth_resource', {
  id: text('id').primaryKey(),
  identifier: text('identifier').notNull().unique(),
  name: text('name').notNull(),
  accessTokenTtl: integer('access_token_ttl'),
  refreshTokenTtl: integer('refresh_token_ttl'),
  signingAlgorithm: text('signing_algorithm'),
  signingKeyId: text('signing_key_id'),
  allowedScopes: json('allowed_scopes'),
  customClaims: json('custom_claims'),
  dpopBoundAccessTokensRequired: integer('dpop_bound_access_tokens_required', { mode: 'boolean' }).default(false),
  disabled: integer('disabled', { mode: 'boolean' }).default(false),
  createdAt: ms('created_at'),
  updatedAt: ms('updated_at'),
  policyVersion: integer('policy_version').default(1),
  metadata: json('metadata'),
});

export const oauthClientResource = sqliteTable(
  'auth_oauth_client_resource',
  {
    id: text('id').primaryKey(),
    clientId: text('client_id').notNull().references(() => oauthClient.clientId, { onDelete: 'cascade' }),
    resourceId: text('resource_id').notNull().references(() => oauthResource.identifier, { onDelete: 'cascade' }),
    metadata: json('metadata'),
    createdAt: ms('created_at'),
  },
  (table) => [
    uniqueIndex('auth_oauth_client_resource_client_resource_uidx').on(table.clientId, table.resourceId),
    index('auth_oauth_client_resource_client_id_idx').on(table.clientId),
    index('auth_oauth_client_resource_resource_id_idx').on(table.resourceId),
  ],
);

export const oauthRefreshToken = sqliteTable(
  'auth_oauth_refresh_token',
  {
    id: text('id').primaryKey(),
    token: text('token').notNull().unique(),
    clientId: text('client_id').notNull().references(() => oauthClient.clientId, { onDelete: 'cascade' }),
    sessionId: text('session_id').references(() => session.id, { onDelete: 'set null' }),
    userId: text('user_id').notNull().references(() => user.id, { onDelete: 'cascade' }),
    referenceId: text('reference_id'),
    authorizationCodeId: text('authorization_code_id'),
    resources: json('resources'),
    requestedUserInfoClaims: json('requested_user_info_claims'),
    expiresAt: ms('expires_at'),
    createdAt: ms('created_at'),
    revoked: ms('revoked'),
    rotatedAt: ms('rotated_at'),
    rotationReplayResponse: text('rotation_replay_response'),
    rotationReplayExpiresAt: ms('rotation_replay_expires_at'),
    authTime: ms('auth_time'),
    confirmation: json('confirmation'),
    scopes: json('scopes').notNull(),
  },
  (table) => [
    index('auth_oauth_refresh_token_client_id_idx').on(table.clientId),
    index('auth_oauth_refresh_token_session_id_idx').on(table.sessionId),
    index('auth_oauth_refresh_token_user_id_idx').on(table.userId),
    index('auth_oauth_refresh_token_authorization_code_id_idx').on(table.authorizationCodeId),
  ],
);

export const oauthAccessToken = sqliteTable(
  'auth_oauth_access_token',
  {
    id: text('id').primaryKey(),
    token: text('token').unique(),
    clientId: text('client_id').notNull().references(() => oauthClient.clientId, { onDelete: 'cascade' }),
    sessionId: text('session_id').references(() => session.id, { onDelete: 'set null' }),
    userId: text('user_id').references(() => user.id, { onDelete: 'cascade' }),
    referenceId: text('reference_id'),
    authorizationCodeId: text('authorization_code_id'),
    resources: json('resources'),
    requestedUserInfoClaims: json('requested_user_info_claims'),
    refreshId: text('refresh_id').references(() => oauthRefreshToken.id, { onDelete: 'cascade' }),
    expiresAt: ms('expires_at'),
    createdAt: ms('created_at'),
    revoked: ms('revoked'),
    confirmation: json('confirmation'),
    scopes: json('scopes').notNull(),
  },
  (table) => [
    index('auth_oauth_access_token_client_id_idx').on(table.clientId),
    index('auth_oauth_access_token_session_id_idx').on(table.sessionId),
    index('auth_oauth_access_token_user_id_idx').on(table.userId),
    index('auth_oauth_access_token_authorization_code_id_idx').on(table.authorizationCodeId),
    index('auth_oauth_access_token_refresh_id_idx').on(table.refreshId),
  ],
);

export const oauthConsent = sqliteTable(
  'auth_oauth_consent',
  {
    id: text('id').primaryKey(),
    clientId: text('client_id').notNull().references(() => oauthClient.clientId, { onDelete: 'cascade' }),
    userId: text('user_id').references(() => user.id, { onDelete: 'cascade' }),
    referenceId: text('reference_id'),
    resources: json('resources'),
    requestedUserInfoClaims: json('requested_user_info_claims'),
    scopes: json('scopes').notNull(),
    createdAt: ms('created_at'),
    updatedAt: ms('updated_at'),
  },
  (table) => [index('auth_oauth_consent_client_id_idx').on(table.clientId), index('auth_oauth_consent_user_id_idx').on(table.userId)],
);

export const oauthClientAssertion = sqliteTable('auth_oauth_client_assertion', {
  id: text('id').primaryKey(),
  expiresAt: ms('expires_at').notNull(),
});

export const userRelations = relations(user, ({ many }) => ({ sessions: many(session), accounts: many(account) }));
export const sessionRelations = relations(session, ({ one }) => ({ user: one(user, { fields: [session.userId], references: [user.id] }) }));
export const accountRelations = relations(account, ({ one }) => ({ user: one(user, { fields: [account.userId], references: [user.id] }) }));
