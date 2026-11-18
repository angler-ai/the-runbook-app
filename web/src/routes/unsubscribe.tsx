import { useSearch } from '@tanstack/react-router';
import { UnsubscribePage } from '../features/auth/UnsubscribePage';

export function UnsubscribeRoute() {
  const search = useSearch({ strict: false }) as { token?: string };
  return <UnsubscribePage token={search.token} />;
}
