// SLACK1 stage 1: the project's Slack incoming-webhook URL. Write-only:
// the API never returns it, so the field starts empty even when set.
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';
import { api } from '../../api/client';
import { Button } from '../../ui/primitives/button';
import { Input } from '../../ui/primitives/input';
import { Label } from '../../ui/primitives/label';

export function SlackSection({ projectRef }: { projectRef: string }) {
  const queryClient = useQueryClient();
  const key = ['projects', projectRef, 'slack'] as const;
  const query = useQuery({
    queryKey: key,
    queryFn: async () => {
      const { data } = await api.GET('/v1/projects/{p}/slack', {
        params: { path: { p: projectRef } },
      });
      return data as { configured: boolean };
    },
  });
  const [url, setUrl] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const save = useMutation({
    mutationFn: async () => {
      const { data } = await api.PUT('/v1/projects/{p}/slack', {
        params: { path: { p: projectRef } },
        body: { url: url.trim() },
      });
      return data as { configured: boolean };
    },
    onSuccess: (data) => {
      queryClient.setQueryData(key, data);
      setUrl('');
      setSaved(true);
    },
    onError: () =>
      setError('That URL was not accepted. Paste a full https://hooks.slack.com/ URL.'),
  });
  const remove = useMutation({
    mutationFn: async () => {
      await api.DELETE('/v1/projects/{p}/slack', { params: { path: { p: projectRef } } });
    },
    onSuccess: () => queryClient.setQueryData(key, { configured: false }),
    onError: () => setError('The Slack webhook could not be removed. Try again.'),
  });

  return (
    <section aria-labelledby="project-slack-heading" className="grid gap-3">
      <div>
        <h2 id="project-slack-heading" className="text-sm font-semibold text-fg">
          Slack notifications
        </h2>
        <p className="text-xs text-fg-muted">
          Posts step assignments, approvals needed, handoffs and failed runs to one channel, each
          linking back to the run. The URL is a credential: it is stored, never shown again.
        </p>
      </div>
      {query.isLoading ? <p role="status">Loading Slack settings…</p> : null}
      {query.data?.configured ? (
        <p role="status">
          A Slack webhook is configured.{' '}
          <Button
            type="button"
            variant="secondary"
            size="sm"
            disabled={remove.isPending}
            onClick={() => {
              setError(null);
              remove.mutate();
            }}
          >
            Remove it
          </Button>
        </p>
      ) : null}
      <form
        className="grid gap-2"
        onSubmit={(event) => {
          event.preventDefault();
          setError(null);
          setSaved(false);
          save.mutate();
        }}
      >
        <Label htmlFor="slack-webhook-url">
          {query.data?.configured ? 'Replace the webhook URL' : 'Incoming webhook URL'}
        </Label>
        <Input
          id="slack-webhook-url"
          type="password"
          autoComplete="off"
          placeholder="https://hooks.slack.com/services/…"
          value={url}
          onChange={(event) => setUrl(event.target.value)}
        />
        <div>
          <Button type="submit" disabled={save.isPending || url.trim().length === 0}>
            {save.isPending ? 'Saving…' : 'Save webhook URL'}
          </Button>
        </div>
      </form>
      {saved ? <p role="status">Slack webhook saved.</p> : null}
      {error ? <p role="alert">{error}</p> : null}
    </section>
  );
}
