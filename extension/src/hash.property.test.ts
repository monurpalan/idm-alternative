// Random page text against the checksum extractor: it never throws, takes linear time, and
// every checksum it gives is one the text holds, of the length its algorithm has.
//
//   HASH_RUNS=2000000 npm test    (longer than the default run)

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { hashesIn, hashForName, pick, type PageHash } from './hash.ts';

const RUNS = Number(process.env.HASH_RUNS ?? 20000);
const LENGTH: Record<string, number> = { md5: 32, sha1: 40, sha256: 64, sha512: 128 };

/** A small seeded generator (mulberry32), so a failure can be run again. */
function random(seed: number) {
	return () => {
		seed = (seed + 0x6d2b79f5) | 0;
		let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
		t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
		return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
	};
}

const PIECES = [
	'SHA-256: ', 'sha256sum ', 'SHA1 ', 'MD5 checksum: ', 'SHA512 (', ') = ', 'sha-1', 'md5', '  ', ' *',
	'\n', '\r\n', ': ', '|', '(', ')', '=', '-', '_', '.', 'file.iso', 'a.zip', 'a.zip.asc', 'é', '‮',
	'\u0000', '𝔘', 'x', '0', 'f', 'commit ',
];

function text(next: () => number): string {
	const hex = (n: number) => Array.from({ length: n }, () => '0123456789abcdefABCDEF'[Math.floor(next() * 22)]).join('');
	const lengths = [31, 32, 33, 40, 63, 64, 65, 128, 129, 1 + Math.floor(next() * 140)];
	let out = '';
	for (let i = Math.floor(next() * 12); i >= 0; i--) {
		out += next() < 0.4 ? hex(lengths[Math.floor(next() * lengths.length)]) : PIECES[Math.floor(next() * PIECES.length)];
	}
	return out;
}

function sound(found: PageHash[], source: string) {
	const lower = source.toLowerCase();
	for (const h of found) {
		assert.equal(h.hex.length, LENGTH[h.algorithm], JSON.stringify({ source, h }));
		assert.match(h.hex, /^[0-9a-f]+$/);
		assert.ok(lower.includes(h.hex), JSON.stringify({ source, h }));
	}
}

test('random text: only checksums the text holds', () => {
	const seed = Number(process.env.HASH_SEED ?? 1);
	const next = random(seed);
	for (let i = 0; i < RUNS; i++) {
		const page = text(next);
		const found = hashesIn(page);
		sound(found, page);
		const best = pick(found);
		assert.ok(best === null || found.includes(best));
		const name = next() < 0.5 ? 'file.iso' : 'a.zip';
		const named = hashForName(page, name);
		if (named) sound([named], page);
	}
});

test('long runs of digits take linear time', () => {
	const page = 'sha256: ' + 'a1'.repeat(200_000) + ' file.iso\n'.repeat(1000);
	const start = performance.now();
	hashesIn(page);
	hashForName(page, 'file.iso');
	assert.ok(performance.now() - start < 2000, 'took too long');
});
