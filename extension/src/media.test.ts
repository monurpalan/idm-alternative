import assert from 'node:assert/strict';
import { test } from 'node:test';
import { MAX_ITEMS, MIN_FILE_BYTES, classify, label, remember, totalSize, type Response } from './media.ts';

const r = (url: string, contentType: string | null, contentLength: number | null = null, contentRange: string | null = null): Response => ({
	url,
	contentType,
	contentLength,
	contentRange
});

test('playlists are recognised by type or extension, without a size', () => {
	assert.equal(classify(r('https://cdn.x/v/master.m3u8', 'application/vnd.apple.mpegurl', 900))?.kind, 'hls');
	assert.equal(classify(r('https://cdn.x/v/x.m3u8?token=1', 'text/plain'))?.kind, 'hls');
	assert.equal(classify(r('https://cdn.x/live', 'application/x-mpegURL'))?.kind, 'hls');
	const dash = classify(r('https://cdn.x/a/stream.mpd', 'application/dash+xml', 4000));
	assert.equal(dash?.kind, 'dash');
	assert.equal(dash?.size, null);
});

test('segments and small files are not offered', () => {
	assert.equal(classify(r('https://cdn.x/seg-001.ts', 'video/mp2t', 2_000_000)), null);
	assert.equal(classify(r('https://cdn.x/chunk-5.m4s', 'video/mp4', 2_000_000)), null);
	assert.equal(classify(r('https://cdn.x/sub.vtt', 'text/vtt', 2000)), null);
	assert.equal(classify(r('https://cdn.x/preview.mp4', 'video/mp4', 300_000)), null, 'too small');
	assert.equal(classify(r('https://x.com/page.html', 'text/html', 5_000_000)), null);
	assert.equal(classify(r('blob:https://x.com/abc', 'video/mp4', 9_000_000)), null);
});

test('files, including ranged fetches by players', () => {
	const v = classify(r('https://cdn.x/movie.mp4', 'video/mp4', 50_000_000));
	assert.deepEqual(v, { url: 'https://cdn.x/movie.mp4', kind: 'video', size: 50_000_000, mime: 'video/mp4' });
	// First chunk of a ranged fetch: listed with the full size.
	const first = classify(r('https://cdn.x/a.webm', 'video/webm', 1_048_576, 'bytes 0-1048575/90000000'));
	assert.equal(first?.size, 90_000_000);
	// Later chunks: not listed again.
	assert.equal(classify(r('https://cdn.x/a.webm', 'video/webm', 1_048_576, 'bytes 1048576-2097151/90000000')), null);
	// Unknown size: still offered.
	assert.equal(classify(r('https://cdn.x/song.mp3', 'audio/mpeg'))?.kind, 'audio');
	// Generic type, telling extension.
	assert.equal(classify(r('https://cdn.x/clip.mkv', 'application/octet-stream', MIN_FILE_BYTES))?.kind, 'video');
	assert.equal(totalSize(r('u', null, 10, 'bytes 0-9/*')), 10);
});

test('the list keeps each URL once, newest last, capped', () => {
	let list = remember([], { url: 'https://a/1.mp4', kind: 'video', size: 1, mime: null });
	list = remember(list, { url: 'https://a/2.mp4', kind: 'video', size: 2, mime: null });
	list = remember(list, { url: 'https://a/1.mp4#t=5', kind: 'video', size: 3, mime: null });
	assert.deepEqual(list.map((m) => m.size), [2, 3]);
	for (let i = 0; i < MAX_ITEMS + 10; i++) list = remember(list, { url: `https://a/${i}.mp4`, kind: 'video', size: i, mime: null });
	assert.equal(list.length, MAX_ITEMS);
	assert.equal(list.at(-1)?.size, MAX_ITEMS + 9);
});

test('labels', () => {
	assert.equal(label({ url: 'https://v.x/a/%C3%A7ay.mp4?x', kind: 'video', size: null, mime: null }), 'çay.mp4');
	assert.equal(label({ url: 'https://v.x/abc/master.m3u8', kind: 'hls', size: null, mime: null }), 'v.x HLS');
	assert.equal(label({ url: 'https://v.x/', kind: 'dash', size: null, mime: null }), 'v.x DASH');
});
