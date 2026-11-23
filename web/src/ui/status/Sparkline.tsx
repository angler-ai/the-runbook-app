export function Sparkline({ series, label }: { series: number[]; label: string }) {
  const width = 120;
  const height = 28;
  const max = Math.max(1, ...series);
  const step = series.length > 1 ? width / (series.length - 1) : width;
  const points = series
    .map(
      (value, i) =>
        `${(i * step).toFixed(1)},${(height - 2 - (value / max) * (height - 4)).toFixed(1)}`,
    )
    .join(' ');
  return (
    <svg
      role="img"
      aria-label={label}
      viewBox={`0 0 ${width} ${height}`}
      className="h-7 w-[120px] overflow-visible text-accent"
    >
      <polyline
        points={points}
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinejoin="round"
      />
    </svg>
  );
}
