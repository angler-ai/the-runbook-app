// UI-1: the theme at boot. Two things decide how the app looks:
// - the person's preference (light, dark or system), kept in this browser;
// - the operator's colour override, from GET /v1/meta `theme`.
// public/theme-boot.js applies both from localStorage before first paint
// (a classic script in <head>, allowed by the CSP's script-src 'self');
// main.tsx then fetches /v1/meta before rendering, applies what it says and
// caches it for the next load. The two files share the keys below.
//
// The cache holds colours, not CSS: `{"light":{"--rb-…":"#rrggbb"},"dark":{…}}`,
// only names matching THEME_VAR_NAME and values matching THEME_VAR_VALUE.
// theme-boot.js validates each again and rebuilds the CSS itself (as
// cssFromVars does), so nothing else in localStorage can reach a <style>.
import { cssFromVars, deriveOverrideVars, sanitizeThemeVars, type ThemeOverride } from './colors';

export type ThemePreference = 'light' | 'dark' | 'system';

export const PREFERENCE_KEY = 'rb.theme';
export const OVERRIDE_KEY = 'rb.theme-vars';
/** The old cache of raw CSS; removed wherever it is seen. */
export const LEGACY_OVERRIDE_KEY = 'rb.theme-css';
export const OVERRIDE_STYLE_ID = 'rb-theme-override';

/** Product surfaces are light by default (Angler); dark is an option. */
export const DEFAULT_PREFERENCE: ThemePreference = 'light';

function storage(): Storage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export function readPreference(): ThemePreference {
  try {
    const value = storage()?.getItem(PREFERENCE_KEY);
    return value === 'light' || value === 'dark' || value === 'system' ? value : DEFAULT_PREFERENCE;
  } catch {
    return DEFAULT_PREFERENCE;
  }
}

/** `system` leaves data-theme unset so tokens.css follows the OS. */
export function applyPreference(preference: ThemePreference, root = document.documentElement) {
  if (preference === 'system') root.removeAttribute('data-theme');
  else root.setAttribute('data-theme', preference);
}

export function savePreference(preference: ThemePreference) {
  try {
    storage()?.setItem(PREFERENCE_KEY, preference);
  } catch {
    // The choice still applies for this page; it just is not remembered.
  }
  applyPreference(preference);
}

/** Replace (or remove, for '') the override stylesheet. */
export function applyOverrideCss(css: string, doc: Document = document) {
  let style = doc.getElementById(OVERRIDE_STYLE_ID);
  if (!css) {
    style?.remove();
    return;
  }
  if (!style) {
    style = doc.createElement('style');
    style.id = OVERRIDE_STYLE_ID;
    doc.head.appendChild(style);
  }
  if (style.textContent !== css) style.textContent = css;
}

interface MetaWithTheme {
  theme?: ThemeOverride | null;
}

/**
 * Fetch /v1/meta (at most `timeoutMs`) and apply its theme. A slow or
 * failing server keeps whatever theme-boot.js applied from the cache, so a
 * known override never flashes back to the default.
 */
export async function loadThemeOverride({
  fetchImpl = fetch,
  timeoutMs = 1500,
}: { fetchImpl?: typeof fetch; timeoutMs?: number } = {}): Promise<void> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const response = await fetchImpl('/v1/meta', {
      signal: controller.signal,
      headers: { accept: 'application/json' },
    });
    if (!response.ok) return;
    const meta = (await response.json()) as MetaWithTheme;
    const vars = sanitizeThemeVars(meta.theme ? deriveOverrideVars(meta.theme) : null);
    const css = cssFromVars(vars);
    applyOverrideCss(css);
    try {
      storage()?.removeItem(LEGACY_OVERRIDE_KEY);
      if (css) storage()?.setItem(OVERRIDE_KEY, JSON.stringify(vars));
      else storage()?.removeItem(OVERRIDE_KEY);
    } catch {
      // Not cached: the next load fetches it again before rendering.
    }
  } catch {
    // Offline or timed out: keep the cached override, if any.
  } finally {
    clearTimeout(timer);
  }
}
