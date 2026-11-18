// NOTIFY1: how much notification email the signed-in person wants.
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';
import { api } from '../../api/client';
import type { components } from '../../api/schema';

type Preference = components['schemas']['EmailPreferenceView'];

const OPTIONS: { value: Preference; label: string; hint: string }[] = [
  {
    value: 'all',
    label: 'Everything',
    hint: 'Approvals, assignments, reminders, escalations and failed runs.',
  },
  { value: 'approvals_only', label: 'Approvals only', hint: 'Only when your approval is needed.' },
  {
    value: 'daily_summary',
    label: 'Daily summary',
    hint: 'One digest instead of a mail per event.',
  },
  { value: 'none', label: 'No email', hint: 'Nothing leaves the app.' },
];

export const emailPreferencesKey = ['me', 'email-preferences'] as const;

export function useEmailPreferences() {
  return useQuery({
    queryKey: emailPreferencesKey,
    queryFn: async () => {
      const { data } = await api.GET('/v1/me/email-preferences', {});
      return data as { preference: Preference };
    },
  });
}

export function EmailPreferencesSection() {
  const queryClient = useQueryClient();
  const query = useEmailPreferences();
  const [error, setError] = useState<string | null>(null);
  const mutation = useMutation({
    mutationFn: async (preference: Preference) => {
      const { data } = await api.PUT('/v1/me/email-preferences', { body: { preference } });
      return data as { preference: Preference };
    },
    onSuccess: (data) => queryClient.setQueryData(emailPreferencesKey, data),
    onError: () => setError('Your preference could not be saved. Try again.'),
  });

  return (
    <section aria-labelledby="email-preferences-heading" className="grid gap-3">
      <div>
        <h2 id="email-preferences-heading" className="text-lg font-semibold">
          Notification email
        </h2>
        <p className="text-sm text-fg-muted">
          When run activity needs you, we can say so by email as well as in My tasks.
        </p>
      </div>
      {query.isLoading ? <p role="status">Loading your preference…</p> : null}
      {query.isError ? <p role="alert">Your preference could not be loaded.</p> : null}
      {error ? <p role="alert">{error}</p> : null}
      <fieldset className="grid gap-2">
        <legend className="sr-only">Notification email preference</legend>
        {OPTIONS.map((option) => (
          <label key={option.value} className="flex items-start gap-2">
            <input
              type="radio"
              name="email-preference"
              value={option.value}
              checked={query.data?.preference === option.value}
              disabled={mutation.isPending || query.isLoading}
              onChange={() => {
                setError(null);
                mutation.mutate(option.value);
              }}
            />
            <span>
              <span className="font-medium">{option.label}</span>{' '}
              <span className="text-fg-muted">{option.hint}</span>
            </span>
          </label>
        ))}
      </fieldset>
    </section>
  );
}
