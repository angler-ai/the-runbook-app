import test from 'node:test';
import assert from 'node:assert/strict';
import { encodeQr, rsDivisor, rsRemainder } from './qr.ts';

const render = (modules: boolean[][]) =>
  modules.map((row) => row.map((d) => (d ? '#' : '.')).join(''));

// `qrencode -l M -8 -m 0 -t ASCII 'otpauth://totp/a?secret=AB'` (libqrencode
// 4.1.1), which chose mask 4 for this input.
const QRENCODE_V2_MASK4 = [
  '#######.#..###.##.#######',
  '#.....#..#.#..#.#.#.....#',
  '#.###.#..#....#...#.###.#',
  '#.###.#.###.###...#.###.#',
  '#.###.#.#..#####..#.###.#',
  '#.....#.#.#..#..#.#.....#',
  '#######.#.#.#.#.#.#######',
  '........##.##.###........',
  '#...#.###..#.....#####..#',
  '###.#..###..#####..##..#.',
  '##....##.##.##.#..#####..',
  '##.###..#.....#...###.##.',
  '###..####.#.#..#.###..###',
  '###.#...#...##.#.#.##..#.',
  '....###...##.#.##...#.#..',
  '.......#..#....###..#.#.#',
  '###.######..#...########.',
  '........#....####...##...',
  '#######.##.##.#.#.#.#.#..',
  '#.....#...##..#.#...#.###',
  '#.###.#.#...#...#######..',
  '#.###.#..#...#...##..#.##',
  '#.###.#....##.#.###..###.',
  '#.....#..#.##.....#...##.',
  '#######.####..##.#.#..###',
];

test('matches an independent encoder module for module', () => {
  const qr = encodeQr('otpauth://totp/a?secret=AB', 4);
  assert.equal(qr.version, 2);
  assert.equal(qr.size, 25);
  assert.deepEqual(render(qr.modules), QRENCODE_V2_MASK4);
});

test('picks the smallest version that fits an authenticator URI', () => {
  const uri =
    'otpauth://totp/The%20Runbook:ada%40example.test?secret=JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP&issuer=The%20Runbook&digits=6&period=30';
  const qr = encodeQr(uri);
  assert.equal(qr.version, 8);
  assert.equal(qr.size, 49);
  assert.ok(qr.mask >= 0 && qr.mask < 8);
  // Finder patterns in three corners, with the dark module beside the bottom-left one.
  for (const [x, y] of [
    [0, 0],
    [qr.size - 7, 0],
    [0, qr.size - 7],
  ] as const) {
    assert.equal(qr.modules[y]![x], true);
    assert.equal(qr.modules[y + 1]![x + 1], false);
    assert.equal(qr.modules[y + 3]![x + 3], true);
  }
  assert.equal(qr.modules[qr.size - 8]![8], true);
});

test('Reed-Solomon codewords leave a zero remainder', () => {
  const data = Array.from({ length: 20 }, (_, i) => (i * 37 + 11) & 0xff);
  const divisor = rsDivisor(16);
  const ecc = rsRemainder(data, divisor);
  assert.equal(ecc.length, 16);
  assert.deepEqual(rsRemainder([...data, ...ecc], divisor), new Array(16).fill(0));
});

test('refuses text longer than version 40 holds', () => {
  assert.throws(() => encodeQr('x'.repeat(3000)), /too long/);
});
