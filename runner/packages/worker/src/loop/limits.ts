export interface LoopLimits {
  maxTurns: number;
  maxTokens: number;
  maxOutputTokens: number;
  timeoutMs: number;
  idleMs: number;
  toolMs: number;
  stallMs: number;
}

export class StepStopped extends Error {
  readonly reason: string;
  constructor(reason: string) {
    super(reason);
    this.reason = reason;
  }
}

export function deadline(ms: number, reason: string): { signal: AbortSignal; touch(): void; close(): void } {
  const controller = new AbortController();
  let timer: ReturnType<typeof setTimeout>;
  function touch() {
    clearTimeout(timer);
    timer = setTimeout(() => controller.abort(new StepStopped(reason)), ms);
  }
  touch();
  return { signal: controller.signal, touch, close: () => clearTimeout(timer) };
}
