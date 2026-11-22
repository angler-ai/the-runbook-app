import { useMemo } from 'react';
import { encodeQr } from './qr';

/**
 * A QR code drawn as inline SVG from a local encoder: the secret in `value`
 * never leaves the page. Dark modules on a light tile in both themes, since
 * phone cameras expect that, with the four-module quiet zone.
 */
export function QrCode({
  value,
  label,
  size = 192,
}: {
  value: string;
  label: string;
  size?: number;
}) {
  const { path, extent } = useMemo(() => {
    const matrix = encodeQr(value);
    const quiet = 4;
    let d = '';
    matrix.modules.forEach((row, y) => {
      row.forEach((dark, x) => {
        if (dark) d += `M${x + quiet} ${y + quiet}h1v1h-1z`;
      });
    });
    return { path: d, extent: matrix.size + quiet * 2 };
  }, [value]);
  return (
    <svg
      role="img"
      aria-label={label}
      data-testid="totp-qr"
      width={size}
      height={size}
      viewBox={`0 0 ${extent} ${extent}`}
      shapeRendering="crispEdges"
      className="rounded-md border border-border"
    >
      <rect width={extent} height={extent} fill="#ffffff" />
      <path d={path} fill="#000000" />
    </svg>
  );
}
