import { useCallback, useState } from 'react';
import { readPreference, savePreference, type ThemePreference } from './boot';

/** The person's light/dark/system choice, applied and remembered on change. */
export function useThemePreference(): [ThemePreference, (next: ThemePreference) => void] {
  const [preference, setPreference] = useState<ThemePreference>(readPreference);
  const change = useCallback((next: ThemePreference) => {
    savePreference(next);
    setPreference(next);
  }, []);
  return [preference, change];
}
