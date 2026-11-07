import assert from 'node:assert/strict';
import { test } from 'node:test';
import { asNoted, qualitiesOf, tiktokItems, videoKey, youtubeFormats, youtubePlayers } from './formats.ts';

/** Cut down from a watch page's `ytInitialPlayerResponse` (2026-11). */
const PLAYER = {
	playabilityStatus: { status: 'OK' },
	videoDetails: { videoId: 'dQw4w9WgXcQ', isLiveContent: false },
	streamingData: {
		expiresInSeconds: '21540',
		formats: [{ itag: 18, mimeType: 'video/mp4; codecs="avc1.42001E, mp4a.40.2"', height: 360, fps: 25 }],
		adaptiveFormats: [
			{ itag: 313, mimeType: 'video/webm; codecs="vp9"', height: 2160, fps: 25, contentLength: '358608461' },
			{ itag: 401, mimeType: 'video/mp4; codecs="av01.0.12M.08"', height: 2160, fps: 25, contentLength: '240334643' },
			{ itag: 137, mimeType: 'video/mp4; codecs="avc1.640028"', height: 1080, fps: 25, contentLength: '80911999' },
			{ itag: 248, mimeType: 'video/webm; codecs="vp9"', height: 1080, fps: 25, contentLength: '30846580' },
			{ itag: 399, mimeType: 'video/mp4; codecs="av01.0.08M.08"', height: 1080, fps: 25, contentLength: '30415996' },
			{ itag: 136, mimeType: 'video/mp4; codecs="avc1.4D401F"', height: 720, fps: 25, contentLength: '26455880' },
			{ itag: 140, mimeType: 'audio/mp4; codecs="mp4a.40.2"', contentLength: '3449447' },
			{ itag: 251, mimeType: 'audio/webm; codecs="opus"', contentLength: '3433755' }
		]
	}
};

/** Cut down from an item of TikTok's `/api/recommend/item_list` (2026-11). */
const TIKTOK_ITEM = {
	id: '7694232242813537543',
	author: { uniqueId: 'hama.mgang' },
	video: {
		id: '7694232242813537543',
		height: 576,
		width: 1024,
		bitrateInfo: [
			{
				Bitrate: 670416,
				BitrateFPS: 24,
				CodecType: 'h265_hvc1',
				PlayAddr: { DataSize: '841540', Width: 1280, Height: 720, UrlKey: 'v14044g5_bytevc1_720p_670416' }
			},
			{
				Bitrate: 648521,
				BitrateFPS: 24,
				CodecType: 'h264',
				PlayAddr: { DataSize: '814056', Width: 1024, Height: 576, UrlKey: 'v14044g5_h264_540p_648521' }
			},
			{
				Bitrate: 560486,
				BitrateFPS: 24,
				CodecType: 'h265_hvc1',
				PlayAddr: { DataSize: '703551', Width: 1024, Height: 576, UrlKey: 'v14044g5_bytevc1_540p_560486' }
			}
		]
	}
};

test("a YouTube player response gives each height once, with yt-dlp's pick's size and audio", () => {
	const n = youtubeFormats(PLAYER);
	assert.equal(n?.key, 'youtube:dQw4w9WgXcQ');
	assert.deepEqual(n?.formats, {
		heights: [
			{ height: 2160, size: 240334643 + 3449447 },
			{ height: 1080, size: 30415996 + 3449447 },
			{ height: 720, size: 26455880 + 3449447 },
			// The muxed 360p has no size of its own here; it still counts as a height.
			{ height: 360 }
		],
		audio: true
	});
	assert.deepEqual(qualitiesOf(n!.formats).slice(0, 2), [
		{ id: '2160', label: '2160p · ~244 MB' },
		{ id: '1080', label: '1080p · ~34 MB' }
	]);
	assert.deepEqual(qualitiesOf(n!.formats).at(-1), { id: 'audio', label: 'Audio only' });
});

test('high frame rates are named', () => {
	const p = {
		...PLAYER,
		streamingData: {
			adaptiveFormats: [
				{ mimeType: 'video/mp4; codecs="avc1.64002a"', height: 1080, fps: 60, contentLength: '2000000' },
				{ mimeType: 'video/mp4; codecs="av01.0.08M.08"', height: 1080, fps: 30, contentLength: '1000000' }
			]
		}
	};
	assert.deepEqual(qualitiesOf(youtubeFormats(p)!.formats), [{ id: '1080', label: '1080p · 60 fps · ~2 MB' }]);
});

test('live, upcoming, unplayable or empty responses are left to yt-dlp', () => {
	assert.equal(youtubeFormats({ ...PLAYER, videoDetails: { videoId: 'dQw4w9WgXcQ', isLive: true } }), null);
	assert.equal(youtubeFormats({ ...PLAYER, playabilityStatus: { status: 'LOGIN_REQUIRED' } }), null);
	assert.equal(youtubeFormats({ ...PLAYER, streamingData: { adaptiveFormats: [] } }), null);
	assert.equal(youtubeFormats({ videoDetails: { videoId: 'x' } }), null);
	assert.equal(youtubeFormats(null), null);
	assert.equal(youtubeFormats('nope'), null);
});

test('player replies are found as they come, plain or wrapped', () => {
	assert.equal(youtubePlayers(PLAYER).length, 1);
	assert.equal(youtubePlayers({ playerResponse: PLAYER, playerConfig: {} }).length, 1);
	assert.equal(youtubePlayers([{ playerResponse: PLAYER }, { other: 1 }]).length, 1);
	assert.equal(youtubePlayers({ adPlacements: [] }).length, 0);
	// The next Shorts come with their players in the list of them.
	const sequence = {
		entries: [{ command: { reelWatchEndpoint: { videoId: 'dQw4w9WgXcQ', unserializedPrefetchData: { playerResponse: PLAYER } } } }]
	};
	assert.equal(youtubeFormats(youtubePlayers(sequence)[0])?.key, 'youtube:dQw4w9WgXcQ');
});

test('a TikTok height only in H.265 is left out when another plays on Windows', () => {
	const [n] = tiktokItems({ itemList: [TIKTOK_ITEM] });
	assert.equal(n.key, 'tiktok:7694232242813537543');
	// 720p comes only in H.265: like `ytdlp::page_info`, it is not offered.
	assert.deepEqual(n.formats, { heights: [{ height: 576, size: 814056 }], audio: false });
	assert.deepEqual(qualitiesOf(n.formats), [{ id: '576', label: '576p · ~814 KB' }]);
});

test('a TikTok video only in H.265 offers what it has', () => {
	const only = { ...TIKTOK_ITEM, video: { bitrateInfo: [TIKTOK_ITEM.video.bitrateInfo[0]] } };
	assert.deepEqual(tiktokItems([only])[0].formats.heights, [{ height: 720, size: 841540 }]);
});

test("TikTok items are found in the page's rehydration data, and the walk is bounded", () => {
	const page = {
		__DEFAULT_SCOPE__: {
			'webapp.app-context': { language: 'tr' },
			'webapp.updated-items': [TIKTOK_ITEM],
			'webapp.video-detail': { itemInfo: { itemStruct: { ...TIKTOK_ITEM, id: '7686150464747982101' } } }
		}
	};
	assert.deepEqual(
		tiktokItems(page).map((n) => n.key),
		['tiktok:7694232242813537543', 'tiktok:7686150464747982101']
	);
	assert.equal(tiktokItems({ itemList: Array(5).fill(TIKTOK_ITEM) }, 3).length, 3);
	let deep: unknown = { itemList: [TIKTOK_ITEM] };
	for (let i = 0; i < 30; i++) deep = { x: deep };
	assert.equal(tiktokItems(deep).length, 0);
	assert.equal(tiktokItems({ itemList: [{ id: '1', video: {} }, null, 'x'] }).length, 0);
});

test("the button's address names the formats' key", () => {
	assert.equal(videoKey('https://www.youtube.com/watch?v=dQw4w9WgXcQ'), 'youtube:dQw4w9WgXcQ');
	assert.equal(videoKey('https://www.youtube.com/shorts/dQw4w9WgXcQ'), 'youtube:dQw4w9WgXcQ');
	assert.equal(videoKey('https://www.tiktok.com/@a.b/video/7694232242813537543'), 'tiktok:7694232242813537543');
	assert.equal(videoKey('https://www.tiktok.com/@/video/7694232242813537543'), 'tiktok:7694232242813537543');
	assert.equal(videoKey('https://www.instagram.com/reel/abc/'), null);
	assert.equal(videoKey(null), null);
});

test('what comes from the page is checked before it is used', () => {
	const n = youtubeFormats(PLAYER)!;
	assert.deepEqual(asNoted(JSON.parse(JSON.stringify(n))), n);
	assert.equal(asNoted({ key: 'evil:1', formats: n.formats }), null);
	assert.equal(asNoted({ key: 'youtube:x', formats: { heights: [{ height: 'big' }] } }), null);
	assert.equal(asNoted({ key: 'youtube:x', formats: { heights: Array(41).fill({ height: 1 }) } }), null);
	assert.equal(asNoted(null), null);
});
