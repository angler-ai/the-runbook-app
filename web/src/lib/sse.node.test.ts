import test from 'node:test';
import assert from 'node:assert/strict';
import { EventStreamParser, readEventStream, type SseEvent } from './sse.ts';

// Helper: parse a string all at once
function parseAll(input: string): SseEvent[] {
  const parser = new EventStreamParser();
  const events = parser.push(input);
  return events.concat(parser.end());
}

// Helper: parse a string split into two chunks at a given offset
function parseSplit(input: string, offset: number): SseEvent[] {
  const parser = new EventStreamParser();
  const chunk1 = input.slice(0, offset);
  const chunk2 = input.slice(offset);
  const events1 = parser.push(chunk1);
  const events2 = parser.push(chunk2);
  const events3 = parser.end();
  return events1.concat(events2, events3);
}

// Helper: parse a string one character at a time
function parseCharByChar(input: string): SseEvent[] {
  const parser = new EventStreamParser();
  const events: SseEvent[] = [];
  for (let i = 0; i < input.length; i++) {
    events.push(...parser.push(input[i] ?? ''));
  }
  events.push(...parser.end());
  return events;
}

// Helper: assert that chunking at every offset and char-by-char produces identical events
function assertChunkInvariance(fixture: string, expected: SseEvent[]) {
  // 1. Single chunk
  const single = parseAll(fixture);
  assert.deepEqual(single, expected, 'single chunk mismatch');

  // 2. Split at every character offset (two chunks)
  for (let i = 0; i <= fixture.length; i++) {
    const splitResult = parseSplit(fixture, i);
    assert.deepEqual(
      splitResult,
      expected,
      `split at offset ${i} failed. chunk1: ${JSON.stringify(fixture.slice(0, i))}, chunk2: ${JSON.stringify(fixture.slice(i))}`
    );
  }

  // 3. One character at a time
  const charResult = parseCharByChar(fixture);
  assert.deepEqual(charResult, expected, 'character-by-character mismatch');
}

test('Rule 1: Line delimiters (LF, CRLF, CR) and CRLF split across chunks', () => {
  // LF
  const lf = 'event: test\ndata: payload\n\n';
  assert.deepEqual(parseAll(lf), [{ event: 'test', data: 'payload' }]);

  // CRLF
  const crlf = 'event: test\r\ndata: payload\r\n\r\n';
  assert.deepEqual(parseAll(crlf), [{ event: 'test', data: 'payload' }]);

  // CR
  const cr = 'event: test\rdata: payload\r\r';
  assert.deepEqual(parseAll(cr), [{ event: 'test', data: 'payload' }]);

  // Mixed delimiters: CRLF on event, LF on data, CR on blank line
  const mixed = 'event: mixed\r\ndata: payload\n\r';
  assert.deepEqual(parseAll(mixed), [{ event: 'mixed', data: 'payload' }]);

  // CRLF split directly between \r and \n across chunks
  const p = new EventStreamParser();
  const e1 = p.push('data: split\r');
  assert.deepEqual(e1, []);
  const e2 = p.push('\n\r\n');
  assert.deepEqual(e2, [{ event: 'message', data: 'split' }]);
  assert.deepEqual(p.end(), []);
});

test('Rule 2: Leading BOM is stripped once at start of stream', () => {
  // With leading BOM
  const withBom = '\uFEFFdata: hello\n\n';
  assert.deepEqual(parseAll(withBom), [{ event: 'message', data: 'hello' }]);

  // BOM in subsequent position is treated as normal character (not stripped)
  const subsequentBom = 'data: first\n\n\uFEFFdata: second\n\n';
  // '\uFEFFdata: second' is unknown field name '\uFEFFdata', so discarded
  const res = parseAll(subsequentBom);
  assert.deepEqual(res, [{ event: 'message', data: 'first' }]);

  // BOM split across chunks
  const p = new EventStreamParser();
  const r1 = p.push('\uFEFF');
  assert.deepEqual(r1, []);
  const r2 = p.push('data: from-bom\n\n');
  assert.deepEqual(r2, [{ event: 'message', data: 'from-bom' }]);
  assert.deepEqual(p.end(), []);
});

test('Rule 3: Lines starting with ":" are comments and are ignored', () => {
  const fixture = ': keep-alive\n\n: another comment\ndata: actual data\n: trailing comment\n\n';
  assertChunkInvariance(fixture, [{ event: 'message', data: 'actual data' }]);
});

test('Rule 4: "field: value" drops exactly ONE leading space; lines without colon have empty value', () => {
  // One space dropped
  const oneSpace = 'data: hello world\n\n';
  assert.deepEqual(parseAll(oneSpace), [{ event: 'message', data: 'hello world' }]);

  // Two spaces: first dropped, second preserved
  const twoSpaces = 'data:  two spaces\n\n';
  assert.deepEqual(parseAll(twoSpaces), [{ event: 'message', data: ' two spaces' }]);

  // Zero spaces after colon
  const zeroSpaces = 'data:no space\n\n';
  assert.deepEqual(parseAll(zeroSpaces), [{ event: 'message', data: 'no space' }]);

  // Line with no colon has field name and empty value
  // 'data\n\n' -> field 'data', value '' -> one empty data line dispatches data ''
  assert.deepEqual(parseAll('data\n\n'), [{ event: 'message', data: '' }]);

  // Line with no colon followed by value
  const multiWithNoColon = 'data: line1\ndata\n\n';
  // line 1: 'line1', line 2: '' -> joined: 'line1\n'
  assert.deepEqual(parseAll(multiWithNoColon), [{ event: 'message', data: 'line1\n' }]);
});

test('Rule 5: Multi-line data joins with "\\n"', () => {
  const fixture = 'data: {\ndata:   "step": "deploy",\ndata:   "status": "ok"\ndata: }\n\n';
  assertChunkInvariance(fixture, [
    {
      event: 'message',
      data: '{\n  "step": "deploy",\n  "status": "ok"\n}',
    },
  ]);
});

test('Rule 6: Event type defaults to "message"', () => {
  const fixture = 'data: default-type\n\n';
  assert.deepEqual(parseAll(fixture), [{ event: 'message', data: 'default-type' }]);

  const explicit = 'event: custom\ndata: payload\n\n';
  assert.deepEqual(parseAll(explicit), [{ event: 'custom', data: 'payload' }]);
});

test('Rule 7: Blank line dispatches; nothing is dispatched if data is empty', () => {
  // Empty data with event type
  assert.deepEqual(parseAll('event: empty\n\n'), []);

  // Empty data with id
  assert.deepEqual(parseAll('id: 1\n\n'), []);

  // A single empty data line still dispatches (WHATWG: only a missing data line is skipped)
  assert.deepEqual(parseAll('data:\n\n'), [{ event: 'message', data: '' }]);
  assert.deepEqual(parseAll('data: \n\n'), [{ event: 'message', data: '' }]);

  // Multiple blank lines in sequence do not emit duplicate empty events
  assert.deepEqual(parseAll('\n\n\n\ndata: test\n\n\n\n'), [{ event: 'message', data: 'test' }]);
});

test('Rule 8: "id" containing NUL is ignored; valid id persists', () => {
  // Valid ID
  const valid = 'id: 100\ndata: event1\n\n';
  assert.deepEqual(parseAll(valid), [{ id: '100', event: 'message', data: 'event1' }]);

  // ID with NUL is ignored
  const nulId = 'id: 10\u0000\ndata: event2\n\n';
  assert.deepEqual(parseAll(nulId), [{ event: 'message', data: 'event2' }]);

  // ID persists across subsequent events if not changed
  const idPersists = 'id: 42\ndata: first\n\ndata: second\n\n';
  assert.deepEqual(parseAll(idPersists), [
    { id: '42', event: 'message', data: 'first' },
    { id: '42', event: 'message', data: 'second' },
  ]);

  // ID can be reset by empty id: field
  const resetId = 'id: 42\ndata: first\n\nid:\ndata: third\n\n';
  assert.deepEqual(parseAll(resetId), [
    { id: '42', event: 'message', data: 'first' },
    { event: 'message', data: 'third' },
  ]);
});

test('Rule 9: "retry" must be all ASCII digits', () => {
  // Valid retry
  const valid = 'retry: 5000\ndata: hello\n\n';
  assert.deepEqual(parseAll(valid), [{ event: 'message', data: 'hello', retry: 5000 }]);

  // Invalid retry with letters
  const invalidLetters = 'retry: 500ms\ndata: hello\n\n';
  assert.deepEqual(parseAll(invalidLetters), [{ event: 'message', data: 'hello' }]);

  // Negative number (contains '-')
  const invalidNegative = 'retry: -100\ndata: hello\n\n';
  assert.deepEqual(parseAll(invalidNegative), [{ event: 'message', data: 'hello' }]);

  // Empty retry
  const invalidEmpty = 'retry:\ndata: hello\n\n';
  assert.deepEqual(parseAll(invalidEmpty), [{ event: 'message', data: 'hello' }]);
});

test('Rule 10: Unknown fields are ignored', () => {
  const fixture = 'foo: bar\nbaz: 123\ndata: test\nqux\n\n';
  assert.deepEqual(parseAll(fixture), [{ event: 'message', data: 'test' }]);
});

test('Rule 11: Partial event at end of stream is discarded', () => {
  // Event missing trailing blank line
  const incomplete = 'data: completed\n\ndata: incomplete';
  assert.deepEqual(parseAll(incomplete), [{ event: 'message', data: 'completed' }]);

  // Missing newline on blank line
  const trailingNoBlank = 'data: test\n';
  assert.deepEqual(parseAll(trailingNoBlank), []);
});

test('Server frame fixtures: matches crates/runbook-server/src/event_stream.rs', () => {
  // Fixture containing all shapes generated by runbook-server:
  // 1. keep-alive comment (: keep-alive)
  // 2. step completion event with id and json payload
  // 3. multi-line event
  // 4. error event
  const serverStream =
    ': keep-alive\n\n' +
    'id: 12\n' +
    'event: execution.step_completed\n' +
    'data: {"step":"deploy","status":"succeeded"}\n\n' +
    ': keep-alive\n\n' +
    'id: 13\n' +
    'event: execution.step_updated\n' +
    'data: {\n' +
    'data:   "step": "verify",\n' +
    'data:   "verdict": "pass"\n' +
    'data: }\n\n' +
    'event: error\n' +
    'data: {"code":"execution.history_expired"}\n\n';

  const expected: SseEvent[] = [
    {
      id: '12',
      event: 'execution.step_completed',
      data: '{"step":"deploy","status":"succeeded"}',
    },
    {
      id: '13',
      event: 'execution.step_updated',
      data: '{\n  "step": "verify",\n  "verdict": "pass"\n}',
    },
    {
      id: '13', // persistent id from previous event
      event: 'error',
      data: '{"code":"execution.history_expired"}',
    },
  ];

  // Test chunk invariance across every single byte offset and char-by-char!
  assertChunkInvariance(serverStream, expected);
});

test('Multi-event fixture chunk invariance across every split offset and char-by-char', () => {
  const fixture =
    'id: 1\n' +
    'event: start\n' +
    'data: begin\n\n' +
    ': middle comment\n\n' +
    'id: 2\n' +
    'event: update\n' +
    'retry: 3000\n' +
    'data: line 1\n' +
    'data: line 2\n\n' +
    'id: 3\n' +
    'data: finish\n\n';

  const expected: SseEvent[] = [
    { id: '1', event: 'start', data: 'begin' },
    { id: '2', event: 'update', data: 'line 1\nline 2', retry: 3000 },
    { id: '3', event: 'message', data: 'finish' },
  ];

  assertChunkInvariance(fixture, expected);
});

test('readEventStream: reads from a ReadableStream using TextDecoder', async () => {
  const encoder = new TextEncoder();
  const streamData = [
    encoder.encode(': initial comment\n\nid: 1\n'),
    encoder.encode('event: execution.step_completed\ndata: {"step":"build"}\n\n'),
    encoder.encode(': keep-alive\n\n'),
    encoder.encode('id: 2\nevent: error\ndata: {"code":"fail"}\n\n'),
  ];

  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const chunk of streamData) {
        controller.enqueue(chunk);
      }
      controller.close();
    },
  });

  const events: SseEvent[] = [];
  for await (const event of readEventStream(stream)) {
    events.push(event);
  }

  assert.deepEqual(events, [
    {
      id: '1',
      event: 'execution.step_completed',
      data: '{"step":"build"}',
    },
    {
      id: '2',
      event: 'error',
      data: '{"code":"fail"}',
    },
  ]);
});

test('readEventStream: handles abort signal', async () => {
  const encoder = new TextEncoder();
  const controller = new AbortController();

  let pulled = 0;
  const stream = new ReadableStream<Uint8Array>({
    pull(c) {
      pulled++;
      if (pulled === 1) {
        c.enqueue(encoder.encode('id: 1\ndata: event1\n\n'));
      } else {
        // Delay before next chunk so abort can trigger
        return new Promise((resolve) => setTimeout(resolve, 50));
      }
    },
  });

  const events: SseEvent[] = [];
  await assert.rejects(
    async () => {
      for await (const event of readEventStream(stream, controller.signal)) {
        events.push(event);
        controller.abort(new Error('Stop stream'));
      }
    },
    { message: 'Stop stream' }
  );

  assert.equal(events.length, 1);
  assert.equal(events[0]?.data, 'event1');
});
