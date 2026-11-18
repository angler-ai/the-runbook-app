/**
 * Server-Sent Events (SSE) stream parser conforming to the WHATWG HTML specification.
 *
 * Rules:
 * - Lines end in CRLF, LF, or CR; CRLF may be split across chunk boundaries.
 * - Leading BOM (U+FEFF) is stripped once at the beginning of the stream.
 * - Lines starting with ':' are comments and are ignored.
 * - 'field: value' drops ONE leading space after the colon; lines without a colon have value ''.
 * - 'data' lines are joined with '\n'.
 * - Event type defaults to 'message'.
 * - A blank line dispatches the event; nothing is dispatched if no data line arrived.
 * - 'id' containing NUL (U+0000) is ignored.
 * - 'retry' must consist only of ASCII digits.
 * - Unknown fields are ignored.
 * - Partial events at the end of the stream are discarded.
 */

export type SseEvent = {
  id?: string;
  event: string;
  data: string;
  retry?: number;
};

export class EventStreamParser {
  private buffer = '';
  private bomStripped = false;
  private lastEventId: string | undefined = undefined;
  private eventType = '';
  private dataLines: string[] = [];
  private currentRetry: number | undefined = undefined;

  /**
   * Push a chunk of text into the parser, returning any dispatched SSE events.
   */
  public push(chunk: string): SseEvent[] {
    if (!chunk) {
      return [];
    }

    this.buffer += chunk;

    // Strip leading BOM once at the very start of the stream
    if (!this.bomStripped) {
      if (this.buffer.startsWith('\uFEFF')) {
        this.buffer = this.buffer.slice(1);
      }
      if (this.buffer.length > 0) {
        this.bomStripped = true;
      }
    }

    const lines = this.extractLines(false);
    return this.processLines(lines);
  }

  /**
   * Signal end of stream. Discards any partial event and flushes completed state.
   */
  public end(): SseEvent[] {
    // Process any remaining complete line ending with a trailing CR
    const lines = this.extractLines(true);
    const events = this.processLines(lines);

    // Discard any partial un-dispatched event
    this.buffer = '';
    this.dataLines = [];
    this.eventType = '';
    this.currentRetry = undefined;

    return events;
  }

  /**
   * Extract complete lines delimited by CRLF, LF, or CR.
   * If isEnd is false and buffer ends in '\r', we wait for the next chunk
   * to determine if it is followed by '\n' (forming CRLF).
   */
  private extractLines(isEnd: boolean): string[] {
    const lines: string[] = [];
    let i = 0;
    let start = 0;

    while (i < this.buffer.length) {
      const code = this.buffer.charCodeAt(i);
      if (code === 10) {
        // \n (LF)
        lines.push(this.buffer.slice(start, i));
        i++;
        start = i;
      } else if (code === 13) {
        // \r (CR)
        if (i + 1 < this.buffer.length) {
          if (this.buffer.charCodeAt(i + 1) === 10) {
            // \r\n (CRLF)
            lines.push(this.buffer.slice(start, i));
            i += 2;
            start = i;
          } else {
            // \r followed by non-\n
            lines.push(this.buffer.slice(start, i));
            i++;
            start = i;
          }
        } else {
          // \r at the very end of buffer
          if (isEnd) {
            // Stream ended; this \r is a complete line break
            lines.push(this.buffer.slice(start, i));
            i++;
            start = i;
          } else {
            // Wait for next chunk to see if \n follows
            break;
          }
        }
      } else {
        i++;
      }
    }

    this.buffer = this.buffer.slice(start);
    return lines;
  }

  /**
   * Process extracted lines according to WHATWG event stream interpretation.
   */
  private processLines(lines: string[]): SseEvent[] {
    const events: SseEvent[] = [];

    for (const line of lines) {
      if (line === '') {
        // Blank line: dispatch event
        const event = this.dispatchEvent();
        if (event) {
          events.push(event);
        }
        continue;
      }

      if (line.charCodeAt(0) === 58) {
        // Starts with ':' -> comment line, ignore
        continue;
      }

      const colonIndex = line.indexOf(':');
      let field: string;
      let value: string;

      if (colonIndex !== -1) {
        field = line.slice(0, colonIndex);
        value = line.slice(colonIndex + 1);
        // Drop ONE leading space if present
        if (value.startsWith(' ')) {
          value = value.slice(1);
        }
      } else {
        field = line;
        value = '';
      }

      switch (field) {
        case 'data':
          this.dataLines.push(value);
          break;
        case 'event':
          this.eventType = value;
          break;
        case 'id':
          // id containing NUL is ignored
          if (!value.includes('\0')) {
            this.lastEventId = value;
          }
          break;
        case 'retry':
          // retry must be all ASCII digits
          if (/^[0-9]+$/.test(value)) {
            this.currentRetry = parseInt(value, 10);
          }
          break;
        default:
          // Unknown field, ignore
          break;
      }
    }

    return events;
  }

  /**
   * Dispatch the current event buffer, resetting eventType, dataLines, and currentRetry.
   * If data buffer is empty, nothing is dispatched.
   */
  private dispatchEvent(): SseEvent | null {
    if (this.dataLines.length === 0) {
      this.eventType = '';
      this.currentRetry = undefined;
      return null;
    }

    // WHATWG: dispatch is skipped only when no data line arrived; a single
    // empty `data:` line dispatches an event whose data is the empty string.
    const data = this.dataLines.join('\n');
    const event: SseEvent = {
      event: this.eventType || 'message',
      data,
    };

    if (this.lastEventId !== undefined && this.lastEventId !== '') {
      event.id = this.lastEventId;
    }

    if (this.currentRetry !== undefined) {
      event.retry = this.currentRetry;
    }

    // Reset event-specific buffers
    this.dataLines = [];
    this.eventType = '';
    this.currentRetry = undefined;

    return event;
  }
}

/**
 * Async generator reading events from a ReadableStream using TextDecoder in stream mode.
 */
export async function* readEventStream(
  body: ReadableStream<Uint8Array>,
  signal?: AbortSignal
): AsyncIterable<SseEvent> {
  signal?.throwIfAborted();

  const parser = new EventStreamParser();
  const decoder = new TextDecoder('utf-8');
  const reader = body.getReader();

  const onAbort = () => {
    reader.cancel(signal?.reason).catch(() => {});
  };
  signal?.addEventListener('abort', onAbort, { once: true });

  try {
    while (true) {
      signal?.throwIfAborted();

      const { done, value } = await reader.read();
      if (done) {
        break;
      }

      if (value && value.byteLength > 0) {
        const text = decoder.decode(value, { stream: true });
        const events = parser.push(text);
        for (const event of events) {
          yield event;
        }
      }
    }

    const trailing = decoder.decode();
    if (trailing) {
      const events = parser.push(trailing);
      for (const event of events) {
        yield event;
      }
    }

    const endEvents = parser.end();
    for (const event of endEvents) {
      yield event;
    }
  } finally {
    signal?.removeEventListener('abort', onAbort);
    reader.releaseLock();
  }
}
