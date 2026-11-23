// SLACK1: where this project's Slack messages go. With the server's Slack
// app (stage 2) a channel ID, whose approval requests carry one button per
// option; without it, an incoming-webhook URL (stage 1), which is
// write-only: the API never returns it, so the field starts empty.
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';
import { api, type Schemas } from '../../api/client';
import { Button } from '../../ui/primitives/button';
import { Input } from '../../ui/primitives/input';
import { Label } from '../../ui/primitives/label';

export function SlackSection({ projectRef }: { projectRef: string }) {
  const queryClient = useQueryClient();
  const key = ['projects', projectRef, 'slack'] as const;
  const query = useQuery({
    queryKey: key,
    queryFn: async () => {
      const { data, error } = await api.GET('/v1/projects/{p}/slack', {
        params: { path: { p: projectRef } },
      });
      if (!data) throw error;
      return data;
    },
  });
  const app = query.data?.app === true;
  const [value, setValue] = useState('');
  const [notifyRunStarted, setNotifyRunStarted] = useState<boolean | null>(null);
  const preferenceChanged =
    notifyRunStarted !== null && notifyRunStarted !== (query.data?.notify_run_started ?? false);
  const canSave =
    value.trim().length > 0 || (query.data?.configured === true && preferenceChanged);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const save = useMutation({
    mutationFn: async () => {
      const target = value.trim();
      const body: Schemas['ProjectSlackRequest'] = target
        ? app
          ? { channel: target }
          : { url: target }
        : {};
      if (notifyRunStarted !== null) body.notify_run_started = notifyRunStarted;
      const { data, error } = await api.PUT('/v1/projects/{p}/slack', {
        params: { path: { p: projectRef } },
        body,
      });
      if (!data) throw error;
      return data;
    },
    onSuccess: (data) => {
      queryClient.setQueryData(key, data);
      setValue('');
      setNotifyRunStarted(null);
      setSaved(true);
    },
    onError: () =>
      setError(
        value.trim().length === 0
          ? 'Could not save run-start notifications. Try again.'
          : app
          ? 'That channel ID was not accepted. Copy it from the channel’s details in Slack (it starts with C or G).'
          : 'That URL was not accepted. Paste a full https://hooks.slack.com/ URL.',
      ),
  });
  const remove = useMutation({
    mutationFn: async () => {
      const { error } = await api.DELETE('/v1/projects/{p}/slack', {
        params: { path: { p: projectRef } },
      });
      if (error) throw error;
    },
    onSuccess: () => {
      setNotifyRunStarted(null);
      queryClient.setQueryData(key, { configured: false, app, notify_run_started: false });
    },
    onError: () => setError('The Slack setting could not be removed. Try again.'),
  });

  const current = query.data?.channel
    ? `Messages go to channel ${query.data.channel}.`
    : query.data?.configured
      ? 'A Slack webhook is configured.'
      : null;

  return (
    <section aria-labelledby="project-slack-heading" className="grid gap-3">
      <div>
        <h2 id="project-slack-heading" className="text-sm font-semibold text-fg">
          Slack notifications
        </h2>
        <p className="text-xs text-fg-muted">
          {app
            ? 'Posts approvals needed, handoffs, reminders and failed runs to one channel. Approval requests have a button per option: a click votes as the person whose Slack email matches their account here. Invite the app to the channel first (/invite in the channel).'
            : 'Posts approvals needed, handoffs, reminders and failed runs to one channel, each linking back to the run. The URL is a credential: it is stored, never shown again. For approve and reject buttons in Slack, the server operator sets up the Slack app.'}
        </p>
      </div>
      {query.isLoading ? <p role="status">Loading Slack settings…</p> : null}
      {current ? (
        <p role="status">
          {current}{' '}
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
          if (canSave) save.mutate();
        }}
      >
        <Label htmlFor="slack-target">
          {app
            ? query.data?.configured
              ? 'Change the channel ID'
              : 'Channel ID'
            : query.data?.configured
              ? 'Replace the webhook URL'
              : 'Incoming webhook URL'}
        </Label>
        <Input
          id="slack-target"
          type={app ? 'text' : 'password'}
          autoComplete="off"
          placeholder={app ? 'C0123ABCDEF' : 'https://hooks.slack.com/services/…'}
          value={value}
          onChange={(event) => setValue(event.target.value)}
        />
        <Label className="flex items-center gap-2">
          <input
            type="checkbox"
            checked={notifyRunStarted ?? query.data?.notify_run_started ?? false}
            onChange={(event) => setNotifyRunStarted(event.target.checked)}
          />
          Notify when a run starts
        </Label>
        <div>
          <Button type="submit" disabled={save.isPending || !canSave}>
            {save.isPending ? 'Saving…' : 'Save Slack settings'}
          </Button>
        </div>
      </form>
      {saved ? <p role="status">Slack setting saved.</p> : null}
      {error ? <p role="alert">{error}</p> : null}
    </section>
  );
}
