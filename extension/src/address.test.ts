import assert from 'node:assert/strict';
import { test } from 'node:test';
import { postAddress, videoAddress, youtubeVideo, type Hints } from './address.ts';

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

test("on any other site, the post a video is in: the link holding its time", () => {
	// X: the tweet's time links to it; the photo and video links name the same tweet.
	const x = hints({
		page: 'https://x.com/home',
		near: ['https://x.com/NASA', 'https://x.com/hashtag/Artemis'],
		post: ['https://x.com/NASA/status/1987654321098765432', 'https://x.com/other/status/1111111111111111111']
	});
	assert.deepEqual(videoAddress(x), { known: false, url: 'https://x.com/NASA/status/1987654321098765432' });
	// A media grid: the video sits inside the link to its tweet.
	const grid = hints({ page: 'https://x.com/NASA/media', around: ['https://x.com/NASA/status/1987654321098765432/video/1'] });
	assert.equal(postAddress(grid), 'https://x.com/NASA/status/1987654321098765432');
	// Reddit: the post's permalink.
	const reddit = hints({ page: 'https://www.reddit.com/', post: ['https://www.reddit.com/r/videos/comments/1abc2de/some_title/'] });
	assert.equal(postAddress(reddit), 'https://www.reddit.com/r/videos/comments/1abc2de');
	// Mastodon and Bluesky.
	assert.equal(postAddress(hints({ page: 'https://mastodon.social/home', post: ['https://mastodon.social/@user/113456789012345678'] })), 'https://mastodon.social/@user/113456789012345678');
	assert.equal(
		postAddress(hints({ page: 'https://bsky.app/', post: ['https://bsky.app/profile/a.bsky.social/post/3kq2xyzab7c2d'] })),
		'https://bsky.app/profile/a.bsky.social/post/3kq2xyzab7c2d'
	);
});

test('no post address from links to profiles, tags or other sites', () => {
	const h = hints({
		page: 'https://x.com/home',
		around: ['https://t.co/abc123XYZ'],
		post: ['https://x.com/NASA', 'https://x.com/user123/likes', 'https://x.com/hashtag/Artemis', 'https://example.com/v/1234567']
	});
	assert.equal(postAddress(h), null);
	assert.deepEqual(videoAddress(h), { known: false, url: null });
});
