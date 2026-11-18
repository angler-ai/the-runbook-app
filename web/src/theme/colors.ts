// UI-1: colour math for the operator's theme override (GET /v1/meta `theme`).
// The server validates `#rrggbb` and warns on low contrast; the web app
// derives the hover, foreground, subtle and line shades from each accent and
// nudges an accent that would fail WCAG AA until it passes, so an override
// can never make text unreadable.

export type Hex = `#${string}`;

const HEX = /^#[0-9a-fA-F]{6}$/;

export function isHex(value: unknown): value is Hex {
  return typeof value === 'string' && HEX.test(value);
}

function rgb(hex: string): [number, number, number] {
  return [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16)) as [number, number, number];
}

function toHex([r, g, b]: [number, number, number]): Hex {
  return `#${[r, g, b]
    .map((c) =>
      Math.round(Math.min(255, Math.max(0, c)))
        .toString(16)
        .padStart(2, '0'),
    )
    .join('')}`;
}

/** WCAG 2 relative luminance. */
export function luminance(hex: string): number {
  const [r, g, b] = rgb(hex).map((c) => {
    const s = c / 255;
    return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  }) as [number, number, number];
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/** WCAG 2 contrast ratio, 1 to 21. */
export function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x) as [number, number];
  return (hi + 0.05) / (lo + 0.05);
}

/** `amount` of `to` mixed into `from` (0 keeps `from`, 1 gives `to`). */
export function mix(from: string, to: string, amount: number): Hex {
  const a = rgb(from);
  const b = rgb(to);
  return toHex([0, 1, 2].map((i) => a[i]! + (b[i]! - a[i]!) * amount) as [number, number, number]);
}

/** Whichever of `options` reads best on `background`. */
export function bestOn(background: string, options: readonly string[]): Hex {
  let best = options[0] as Hex;
  for (const option of options) {
    if (contrast(option, background) > contrast(best, background)) best = option as Hex;
  }
  return best;
}

/**
 * `color`, moved toward `toward` in small steps until it reaches `ratio`
 * against every one of `backgrounds` (or `toward` itself, if nothing short
 * of it does).
 */
export function ensureContrast(
  color: string,
  backgrounds: readonly string[],
  ratio: number,
  toward: string,
): Hex {
  for (let step = 0; step <= 50; step++) {
    const candidate = mix(color, toward, step / 50);
    if (backgrounds.every((bg) => contrast(candidate, bg) >= ratio)) return candidate;
  }
  return toward as Hex;
}

/** AA for body text (WCAG 2.1 1.4.3). */
const AA = 4.5;

/**
 * A hover shade near `shade` that white or ink text reads on at AA, and that
 * text colour. `shade` is moved toward white (the signature lighter hover)
 * or back toward `base` (a shade already known to pass), whichever needs the
 * smaller step; either end passes, so this always succeeds.
 */
export function readableShade(shade: string, base: string): { bg: Hex; fg: Hex } {
  for (let step = 0; step <= 50; step++) {
    for (const toward of [WHITE, base]) {
      const bg = mix(shade, toward, step / 50);
      const fg = bestOn(bg, [WHITE, INK]);
      if (contrast(fg, bg) >= AA) return { bg, fg };
    }
  }
  return { bg: WHITE, fg: INK };
}

export const INK: Hex = '#14133a';
export const WHITE: Hex = '#ffffff';
const BLACK: Hex = '#000000';

export type ThemeName = 'light' | 'dark';

export interface ThemeColorsInput {
  accent?: string | null;
  accent_secondary?: string | null;
}

/**
 * The surfaces each theme's text sits on (tokens.css): an accent must reach
 * 4.5:1 on all of them, since it is also link and selected-row text.
 */
const SURFACES: Record<ThemeName, readonly Hex[]> = {
  light: ['#ffffff', '#f8fafc', '#f1f5f9'],
  dark: ['#0e0f14', '#15161d', '#1a1c24', '#20222c'],
};

/** The --rb-* custom properties one theme's override sets. */
export function deriveThemeVars(theme: ThemeName, input: ThemeColorsInput): Record<string, string> {
  const vars: Record<string, string> = {};
  const surfaces = SURFACES[theme];
  const ground = surfaces[0]!;
  // Light surfaces push a weak accent darker; the dark ground pushes it lighter.
  const toward = theme === 'light' ? BLACK : WHITE;
  if (isHex(input.accent)) {
    const accent = ensureContrast(input.accent.toLowerCase(), surfaces, AA, toward);
    const fg = bestOn(accent, [WHITE, INK]);
    // Angler's signature hover is lighter, not darker; a mid-tone hover that
    // neither white nor ink reads on at AA is moved until one does.
    const hover = readableShade(mix(accent, WHITE, theme === 'light' ? 0.45 : 0.3), accent);
    const strong = ensureContrast(mix(accent, toward, 0.18), surfaces, 4.5, toward);
    let subtleAmount = theme === 'light' ? 0.08 : 0.22;
    let subtle = mix(ground, accent, subtleAmount);
    while (contrast(accent, subtle) < 4.5 && subtleAmount > 0.01) {
      subtleAmount -= 0.02;
      subtle = mix(ground, accent, subtleAmount);
    }
    Object.assign(vars, {
      '--rb-color-accent': accent,
      '--rb-color-accent-fg': fg,
      '--rb-color-accent-hover': hover.bg,
      '--rb-color-accent-hover-fg': hover.fg,
      '--rb-color-accent-strong': strong,
      '--rb-color-accent-subtle': subtle,
      '--rb-color-accent-line': mix(ground, accent, theme === 'light' ? 0.3 : 0.45),
      '--rb-color-focus': accent,
      '--rb-status-running-fg': ensureContrast(accent, [subtle], 4.5, toward),
      '--rb-status-running-bg': subtle,
    });
  }
  if (isHex(input.accent_secondary)) {
    const secondary = input.accent_secondary.toLowerCase();
    Object.assign(vars, {
      '--rb-color-accent-secondary': secondary,
      '--rb-color-accent-secondary-text': ensureContrast(secondary, surfaces, 4.5, toward),
      '--rb-color-accent-secondary-subtle': mix(ground, secondary, theme === 'light' ? 0.18 : 0.25),
    });
  }
  return vars;
}

export interface ThemeOverride {
  light?: ThemeColorsInput | null;
  dark?: ThemeColorsInput | null;
}

/** The derived variables for both themes: what boot.ts caches for theme-boot.js. */
export interface ThemeVars {
  light: Record<string, string>;
  dark: Record<string, string>;
}

/** A cacheable variable name and value (theme-boot.js checks the same). */
export const THEME_VAR_NAME = /^--rb-[a-z0-9-]+$/;
export const THEME_VAR_VALUE = /^#[0-9a-f]{6}$/;

/** Only `--rb-*` names with `#rrggbb` (lowercase) values; anything else is dropped. */
export function sanitizeThemeVars(value: unknown): ThemeVars {
  const pick = (vars: unknown): Record<string, string> => {
    const out: Record<string, string> = {};
    if (!vars || typeof vars !== 'object' || Array.isArray(vars)) return out;
    for (const [name, v] of Object.entries(vars)) {
      if (THEME_VAR_NAME.test(name) && typeof v === 'string' && THEME_VAR_VALUE.test(v))
        out[name] = v;
    }
    return out;
  };
  const record = value && typeof value === 'object' ? (value as Partial<ThemeVars>) : {};
  return { light: pick(record.light), dark: pick(record.dark) };
}

export function deriveOverrideVars(theme: ThemeOverride | null | undefined): ThemeVars {
  return {
    light: deriveThemeVars('light', theme?.light ?? {}),
    dark: deriveThemeVars('dark', theme?.dark ?? {}),
  };
}

function block(selector: string, vars: Record<string, string>): string {
  const body = Object.entries(vars)
    .map(([name, value]) => `${name}:${value};`)
    .join('');
  return `${selector}{${body}}`;
}

/**
 * The stylesheet for derived variables, or '' when there are none. Selectors
 * mirror tokens.css so the dark rules win only where the dark theme is in
 * force, and on screen only (print uses the light theme). public/theme-boot.js builds the same text from the cache.
 */
export function cssFromVars({ light, dark }: ThemeVars): string {
  const parts: string[] = [];
  if (Object.keys(light).length) parts.push(block(':root', light));
  if (Object.keys(dark).length) {
    parts.push(
      `@media screen and (prefers-color-scheme: dark){${block(':root:not([data-theme="light"])', dark)}}`,
      `@media screen{${block(':root[data-theme="dark"]', dark)}}`,
    );
  }
  return parts.join('\n');
}

/** The stylesheet that applies an override, or '' when there is nothing to apply. */
export function overrideCss(theme: ThemeOverride | null | undefined): string {
  if (!theme) return '';
  return cssFromVars(deriveOverrideVars(theme));
}
