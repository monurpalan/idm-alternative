import assert from 'node:assert/strict';
import { test } from 'node:test';
import { videoAddress, youtubeVideo, type Hints } from './address.ts';

const hints = (h: Partial<Hints>): Hints => ({ page: '', around: [], near: [], ids: [], ...h });

test('a YouTube preview on the home page or a search is its own video, not the page', () => {
	const a = videoAddress(
		hints({
			page: 'https://www.youtube.com/',
			around: ['https://www.youtube.com/watch?v=nvNQp8_VZJk&pp=ygUKY3My'],
			near: ['https://www.youtube.com/watch?v=Ml8ESJrxJno']
		})
	);
	assert.deepEqual(a, { known: true, url: 'https://www.youtube.com/watch?v=nvNQp8_VZJk' });
});

test("a watch page's player is the page's video, not the links at its end", () => {
	const a = videoAddress(
		hints({
			page: 'https://www.youtube.com/watch?v=0dp0CGCaXZg&t=30s',
			near: ['https://www.youtube.com/watch?v=Ml8ESJrxJno']
		})
	);
	assert.equal(a.url, 'https://www.youtube.com/watch?v=0dp0CGCaXZg');
});

test('a YouTube feed with no link by the video sends nothing rather than the whole feed', () => {
	assert.deepEqual(videoAddress(hints({ page: 'https://www.youtube.com/' })), { known: true, url: null });
});

test('YouTube addresses in their forms', () => {
	assert.equal(youtubeVideo('https://youtu.be/0dp0CGCaXZg?t=1'), 'https://www.youtube.com/watch?v=0dp0CGCaXZg');
	assert.equal(youtubeVideo('https://www.youtube.com/shorts/0dp0CGCaXZg'), 'https://www.youtube.com/shorts/0dp0CGCaXZg');
	assert.equal(youtubeVideo('https://m.youtube.com/live/0dp0CGCaXZg'), 'https://www.youtube.com/watch?v=0dp0CGCaXZg');
	assert.equal(youtubeVideo('https://www.youtube.com/@channel'), null);
	assert.equal(youtubeVideo('https://notyoutube.com/watch?v=0dp0CGCaXZg'), null);
});

test("a TikTok feed's video is the one whose player box the video is in", () => {
	const a = videoAddress(
		hints({
			page: 'https://www.tiktok.com/foryou',
			ids: ['xgwrapper-0-7659734760415120660', 'media-card-0', 'one-column-item-0', 'column-list-container'],
			near: ['https://www.tiktok.com/@elcinzz', 'https://www.tiktok.com/music/orijinal-ses-7256814377895496453']
		})
	);
	assert.equal(a.url, 'https://www.tiktok.com/@elcinzz/video/7659734760415120660');
	// Without the author's link yt-dlp still finds it by the id.
	const b = videoAddress(hints({ page: 'https://www.tiktok.com/', ids: ['xgwrapper-0-7659734760415120660'] }));
	assert.equal(b.url, 'https://www.tiktok.com/@/video/7659734760415120660');
});

test('a TikTok video page, and a feed with nothing to go by', () => {
	const page = 'https://www.tiktok.com/@elcinzz/video/7659734760415120660?lang=tr';
	assert.equal(videoAddress(hints({ page })).url, 'https://www.tiktok.com/@elcinzz/video/7659734760415120660');
	assert.deepEqual(videoAddress(hints({ page: 'https://www.tiktok.com/foryou' })), { known: true, url: null });
});

test('Instagram reels, and other sites left to the usual way', () => {
	const a = videoAddress(hints({ page: 'https://www.instagram.com/', near: ['https://www.instagram.com/someone/reel/C1a2B3c4D5e/'] }));
	assert.equal(a.url, 'https://www.instagram.com/reel/C1a2B3c4D5e/');
	assert.deepEqual(videoAddress(hints({ page: 'https://vimeo.com/123' })), { known: false, url: null });
});
