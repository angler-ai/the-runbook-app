import { useState } from 'react';
import { EmailPreferencesSection } from './EmailPreferencesSection';
import { ProfileSection } from '../profile/ProfileSection';
import { Button } from '../../ui/primitives/button';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '../../ui/primitives/tabs';
import { isCookieSession, isPatSession } from '../auth/session';
import { CreatePersonalTokenDialog } from './CreatePersonalTokenDialog';
import {
  friendlySettingsError,
  useConnectedApps,
  usePersonalTokens,
  useRevokeConnectedApp,
  useRevokeOtherSessions,
  useRevokeSession,
  useRevokeToken,
  useSessions,
} from './hooks';

export function AccountSettingsPage() {
  const [tab, setTab] = useState<'sessions' | 'tokens' | 'apps'>('sessions');
  const [createTokenOpen, setCreateTokenOpen] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  const isPat = isPatSession();
  const isCookie = isCookieSession();

  const sessionsQuery = useSessions();
  const tokensQuery = usePersonalTokens();
  const appsQuery = useConnectedApps();

  const revokeSessionMutation = useRevokeSession();
  const revokeOtherSessionsMutation = useRevokeOtherSessions();
  const revokeTokenMutation = useRevokeToken();
  const revokeAppMutation = useRevokeConnectedApp();

  const handleRevokeSession = async (token: string) => {
    setActionError(null);
    try {
      await revokeSessionMutation.mutateAsync(token);
    } catch (err) {
      setActionError(friendlySettingsError(err));
    }
  };

  const handleRevokeOtherSessions = async () => {
    setActionError(null);
    try {
      await revokeOtherSessionsMutation.mutateAsync();
    } catch (err) {
      setActionError(friendlySettingsError(err));
    }
  };

  const handleRevokeToken = async (id: string) => {
    setActionError(null);
    try {
      await revokeTokenMutation.mutateAsync(id);
    } catch (err) {
      setActionError(friendlySettingsError(err));
    }
  };

  const handleRevokeApp = async (clientId: string) => {
    setActionError(null);
    try {
      await revokeAppMutation.mutateAsync(clientId);
    } catch (err) {
      setActionError(friendlySettingsError(err));
    }
  };

  return (
    <main className="w-full">
      <div className="mb-6 grid gap-1">
        <h1 className="text-xl font-semibold text-fg">Account settings</h1>
        <p className="text-sm text-fg-muted">
          Your profile, email notifications, sessions, personal tokens and connected apps.
        </p>
      </div>

      {actionError ? (
        <div
          role="alert"
          className="mb-4 rounded-md border border-border bg-surface px-4 py-3 text-sm text-fg"
        >
          {actionError}
        </div>
      ) : null}

      <div className="mb-6 rounded-md border border-border bg-surface p-4">
        <ProfileSection />
      </div>

      <div className="mb-6 rounded-md border border-border bg-surface p-4">
        <EmailPreferencesSection />
      </div>

      <Tabs value={tab} onValueChange={(val) => setTab(val as typeof tab)}>
        <TabsList className="mb-6">
          <TabsTrigger value="sessions">Sessions</TabsTrigger>
          <TabsTrigger value="tokens">Personal tokens</TabsTrigger>
          <TabsTrigger value="apps">Connected apps</TabsTrigger>
        </TabsList>

        {/* Sessions Tab */}
        <TabsContent value="sessions" className="grid gap-4">
          <div className="flex items-center justify-between">
            <div>
              <h2 className="text-base font-semibold text-fg">Active sessions</h2>
              <p className="text-xs text-fg-muted">
                Devices and browsers that currently hold a live session for your account.
              </p>
            </div>
            {sessionsQuery.data && sessionsQuery.data.length > 1 ? (
              <Button
                type="button"
                variant="secondary"
                size="sm"
                disabled={revokeOtherSessionsMutation.isPending}
                onClick={() => void handleRevokeOtherSessions()}
              >
                {revokeOtherSessionsMutation.isPending ? 'Revoking…' : 'Revoke all other sessions'}
              </Button>
            ) : null}
          </div>

          {sessionsQuery.isLoading ? (
            <p className="text-sm text-fg-muted">Loading sessions…</p>
          ) : sessionsQuery.isError ? (
            <p role="alert" className="text-sm text-fg">
              Failed to load active sessions.
            </p>
          ) : !sessionsQuery.data || sessionsQuery.data.length === 0 ? (
            <div className="rounded-md border border-border bg-surface p-6 text-center text-sm text-fg-muted">
              No active sessions recorded.
            </div>
          ) : (
            <div className="grid gap-3">
              {sessionsQuery.data.map((sess) => (
                <div
                  key={sess.id}
                  className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-border bg-surface p-4"
                >
                  <div className="grid gap-1">
                    <span className="font-medium text-sm text-fg">
                      {sess.userAgent || 'Web browser'}
                    </span>
                    <div className="flex flex-wrap gap-2 text-xs text-fg-muted">
                      {sess.ipAddress ? <span>IP: {sess.ipAddress}</span> : null}
                      <span>Created: {new Date(sess.createdAt).toLocaleDateString()}</span>
                    </div>
                  </div>
                  <Button
                    type="button"
                    variant="secondary"
                    size="sm"
                    disabled={revokeSessionMutation.isPending}
                    onClick={() => void handleRevokeSession(sess.token)}
                  >
                    Revoke
                  </Button>
                </div>
              ))}
            </div>
          )}
        </TabsContent>

        {/* Personal Tokens Tab */}
        <TabsContent value="tokens" className="grid gap-4">
          <div className="flex items-center justify-between">
            <div>
              <h2 className="text-base font-semibold text-fg">Personal tokens</h2>
              <p className="text-xs text-fg-muted">
                Tokens granting API or CLI access on your behalf.
              </p>
            </div>
            {isCookie ? (
              <Button type="button" size="sm" onClick={() => setCreateTokenOpen(true)}>
                Generate new token
              </Button>
            ) : null}
          </div>

          {isPat ? (
            <div role="status" className="rounded-md border border-border bg-surface p-6 text-sm">
              <h3 className="font-semibold text-fg">
                Sign in with email or Google to manage tokens (F90)
              </h3>
              <p className="mt-1 text-fg-muted">
                You are currently signed in with a personal access token. Creating and managing
                personal tokens requires signing in with email or Google.
              </p>
            </div>
          ) : tokensQuery.isLoading ? (
            <p className="text-sm text-fg-muted">Loading tokens…</p>
          ) : tokensQuery.isError ? (
            <p role="alert" className="text-sm text-fg">
              Failed to load personal tokens.
            </p>
          ) : !tokensQuery.data || tokensQuery.data.length === 0 ? (
            <div className="rounded-md border border-border bg-surface p-6 text-center text-sm text-fg-muted">
              No personal tokens yet. Generate one for CLI access or scripts.
            </div>
          ) : (
            <div className="grid gap-3">
              {tokensQuery.data.map((tok) => (
                <div
                  key={tok.id}
                  className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-border bg-surface p-4"
                >
                  <div className="grid gap-1">
                    <div className="flex items-center gap-2">
                      <span className="font-semibold text-sm text-fg">{tok.name}</span>
                      <span className="font-mono text-xs text-fg-muted">{tok.id}</span>
                    </div>
                    <div className="flex flex-wrap gap-1 pt-1">
                      {tok.scopes.map((scope) => (
                        <span
                          key={scope}
                          className="rounded border border-border bg-bg px-1.5 py-0.5 font-mono text-2xs text-fg"
                        >
                          {scope}
                        </span>
                      ))}
                    </div>
                    <div className="flex flex-wrap gap-3 pt-1 text-xs text-fg-muted">
                      {tok.created_at_ms ? (
                        <span>Created {new Date(tok.created_at_ms).toLocaleDateString()}</span>
                      ) : null}
                      {tok.expires_at_ms ? (
                        <span>Expires {new Date(tok.expires_at_ms).toLocaleDateString()}</span>
                      ) : null}
                      {tok.last_used_at_ms ? (
                        <span>Last used {new Date(tok.last_used_at_ms).toLocaleDateString()}</span>
                      ) : (
                        <span>Never used</span>
                      )}
                    </div>
                  </div>
                  <Button
                    type="button"
                    variant="secondary"
                    size="sm"
                    disabled={revokeTokenMutation.isPending}
                    onClick={() => void handleRevokeToken(tok.id)}
                  >
                    Revoke
                  </Button>
                </div>
              ))}
            </div>
          )}

          <CreatePersonalTokenDialog open={createTokenOpen} onOpenChange={setCreateTokenOpen} />
        </TabsContent>

        {/* Connected Apps Tab */}
        <TabsContent value="apps" className="grid gap-4">
          <div>
            <h2 className="text-base font-semibold text-fg">Connected applications</h2>
            <p className="text-xs text-fg-muted">
              Remote MCP clients authorized via OAuth 2.1 to access The Runbook tools.
            </p>
          </div>

          {appsQuery.isLoading ? (
            <p className="text-sm text-fg-muted">Loading connected applications…</p>
          ) : appsQuery.isError ? (
            <p role="alert" className="text-sm text-fg">
              Failed to load connected applications.
            </p>
          ) : !appsQuery.data || appsQuery.data.length === 0 ? (
            <div className="rounded-md border border-border bg-surface p-6 text-center text-sm text-fg-muted">
              No connected apps. Remote MCP clients that you authorize will appear here.
            </div>
          ) : (
            <div className="grid gap-3">
              {appsQuery.data.map((app) => (
                <div
                  key={app.client_id}
                  className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-border bg-surface p-4"
                >
                  <div className="grid gap-1">
                    <div className="flex items-center gap-2">
                      <span className="font-semibold text-sm text-fg">
                        {app.client_name || app.client_id}
                      </span>
                      {app.client_name ? (
                        <span className="font-mono text-xs text-fg-muted">{app.client_id}</span>
                      ) : null}
                    </div>
                    {app.redirect_hosts.length > 0 ? (
                      <p className="text-xs text-fg-muted">
                        Redirects: {app.redirect_hosts.join(', ')}
                      </p>
                    ) : null}
                    <div className="flex flex-wrap gap-1 pt-1">
                      {app.scopes.map((scope) => (
                        <span
                          key={scope}
                          className="rounded border border-border bg-bg px-1.5 py-0.5 font-mono text-2xs text-fg"
                        >
                          {scope}
                        </span>
                      ))}
                    </div>
                    <div className="flex flex-wrap gap-3 pt-1 text-xs text-fg-muted">
                      <span>Authorized {new Date(app.created_at_ms).toLocaleDateString()}</span>
                      <span>Expires {new Date(app.expires_at_ms).toLocaleDateString()}</span>
                      {app.last_used_at_ms ? (
                        <span>Last token {new Date(app.last_used_at_ms).toLocaleDateString()}</span>
                      ) : null}
                    </div>
                  </div>
                  <Button
                    type="button"
                    variant="secondary"
                    size="sm"
                    disabled={revokeAppMutation.isPending}
                    onClick={() => void handleRevokeApp(app.client_id)}
                  >
                    Revoke access
                  </Button>
                </div>
              ))}
            </div>
          )}
        </TabsContent>
      </Tabs>
    </main>
  );
}
