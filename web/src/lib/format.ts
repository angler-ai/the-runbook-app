/**
 * Pure time and duration formatters (A2).
 * Formats durations, relative times, and deadline remaining time without external libraries.
 */

/**
 * Formats a duration in milliseconds into a concise human-readable string.
 *
 * Examples:
 *   formatDuration(0) => '0s'
 *   formatDuration(500) => '<1s'
 *   formatDuration(12_000) => '12s'
 *   formatDuration(72_000) => '1m 12s'
 *   formatDuration(60_000) => '1m'
 *   formatDuration(3_660_000) => '1h 1m'
 *   formatDuration(7_200_000) => '2h'
 *   formatDuration(90_000_000) => '1d 1h'
 *   formatDuration(-72_000) => '-1m 12s'
 */
export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms)) return '0s';
  if (ms === 0) return '0s';
  if (ms < 0) return `-${formatDuration(-ms)}`;
  if (ms < 1000) return '<1s';

  const totalSeconds = Math.floor(ms / 1000);
  const days = Math.floor(totalSeconds / 86400);
  const hours = Math.floor((totalSeconds % 86400) / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;

  if (days > 0) {
    return hours > 0 ? `${days}d ${hours}h` : `${days}d`;
  }
  if (hours > 0) {
    return minutes > 0 ? `${hours}h ${minutes}m` : `${hours}h`;
  }
  if (minutes > 0) {
    return seconds > 0 ? `${minutes}m ${seconds}s` : `${minutes}m`;
  }
  return `${seconds}s`;
}

/**
 * Formats a target time relative to `now`.
 *
 * Examples:
 *   formatRelativeTime(now) => 'just now'
 *   formatRelativeTime(now - 500) => 'just now'
 *   formatRelativeTime(now - 45_000) => '45s ago'
 *   formatRelativeTime(now - 1_080_000) => '18m ago'
 *   formatRelativeTime(now - 7_200_000) => '2h ago'
 *   formatRelativeTime(now - 86_400_000) => '1d ago'
 *   formatRelativeTime(now + 300_000) => 'in 5m'
 */
export function formatRelativeTime(
  target: number | Date | string,
  now: number | Date = Date.now(),
): string {
  const targetMs = typeof target === 'number' ? target : new Date(target).getTime();
  const nowMs = typeof now === 'number' ? now : new Date(now).getTime();

  if (!Number.isFinite(targetMs) || !Number.isFinite(nowMs)) {
    return '';
  }

  const diffMs = nowMs - targetMs;

  if (diffMs >= 0) {
    if (diffMs < 5000) return 'just now';
    const totalSeconds = Math.floor(diffMs / 1000);
    if (totalSeconds < 60) return `${totalSeconds}s ago`;
    const minutes = Math.floor(totalSeconds / 60);
    if (minutes < 60) return `${minutes}m ago`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.floor(hours / 24);
    return `${days}d ago`;
  }

  const futureDiff = -diffMs;
  if (futureDiff < 5000) return 'in a few seconds';
  const totalSeconds = Math.floor(futureDiff / 1000);
  if (totalSeconds < 60) return `in ${totalSeconds}s`;
  const minutes = Math.floor(totalSeconds / 60);
  if (minutes < 60) return `in ${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `in ${hours}h`;
  const days = Math.floor(hours / 24);
  return `in ${days}d`;
}

/**
 * Formats time remaining until a deadline.
 * If the deadline has already passed (<= 0), returns 'expired' or 'overdue by ...'.
 *
 * Examples:
 *   formatDeadlineLeft(now) => 'expired'
 *   formatDeadlineLeft(now + 500) => '<1s left'
 *   formatDeadlineLeft(now + 45_000) => '45s left'
 *   formatDeadlineLeft(now + 1_800_000) => '30m left'
 *   formatDeadlineLeft(now + 7_200_000) => '2h left'
 *   formatDeadlineLeft(now + 172_800_000) => '2d left'
 *   formatDeadlineLeft(now - 300_000) => 'overdue by 5m'
 */
export function formatDeadlineLeft(
  deadline: number | Date | string,
  now: number | Date = Date.now(),
): string {
  const deadlineMs = typeof deadline === 'number' ? deadline : new Date(deadline).getTime();
  const nowMs = typeof now === 'number' ? now : new Date(now).getTime();

  if (!Number.isFinite(deadlineMs) || !Number.isFinite(nowMs)) {
    return '';
  }

  const remaining = deadlineMs - nowMs;

  if (remaining === 0) {
    return 'expired';
  }

  if (remaining < 0) {
    const overdueMs = -remaining;
    if (overdueMs < 1000) return 'expired';
    const totalSeconds = Math.floor(overdueMs / 1000);
    if (totalSeconds < 60) return `overdue by ${totalSeconds}s`;
    const minutes = Math.floor(totalSeconds / 60);
    if (minutes < 60) return `overdue by ${minutes}m`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `overdue by ${hours}h`;
    const days = Math.floor(hours / 24);
    return `overdue by ${days}d`;
  }

  if (remaining < 1000) return '<1s left';
  const totalSeconds = Math.floor(remaining / 1000);
  if (totalSeconds < 60) return `${totalSeconds}s left`;
  const minutes = Math.floor(totalSeconds / 60);
  if (minutes < 60) return `${minutes}m left`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h left`;
  const days = Math.floor(hours / 24);
  return `${days}d left`;
}
