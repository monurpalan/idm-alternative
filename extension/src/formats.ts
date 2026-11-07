// The qualities a video offers, read from what its site already gave the page, as IDM does:
// YouTube's player response (`ytInitialPlayerResponse`, `/youtubei/v1/player`) and TikTok's
// feed items (`itemList`, `__UNIVERSAL_DATA_FOR_REHYDRATION__`). The menu by a video then opens
// at once instead of after yt-dlp's few seconds; the download still goes through yt-dlp with
// the height picked. Pure, tested with node:test; page-data.ts feeds it, video-button.ts uses it.

/** One height the menu offers. */
export interface Offer {
	height: number;
	/** Frames per second, when above 30. */
	fps?: number;
	/** Bytes of the file it makes (video and audio), when the site says. */
	size?: number;
}

/** What a video offers: its heights, best first, and whether audio comes alone. */
export interface Formats {
	heights: Offer[];
	audio: boolean;
}

/** A video's formats under its key (see `videoKey`). */
export interface Noted {
	key: string;
	formats: Formats;
}

/** One quality the menu lists; the same as the app's (`video::Quality`). */
export interface Quality {
	id: string;
	label: string;
}

/**
 * Codecs Windows cannot play without a paid extension; the same as
 * `shardload_manager::ytdlp::UNPLAYABLE`.
 */
const UNPLAYABLE = ['hev', 'hvc', 'h265', 'bytevc1'];

export function playable(codec: string): boolean {
	const c = codec.toLowerCase();
	return !UNPLAYABLE.some((u) => c.startsWith(u));
}

/** A height the site offers, in one codec. */
interface Format {
	height: number;
	fps: number;
	codec: string;
	size: number | null;
	/** How much yt-dlp would like it among those of its height and frame rate. */
	rank: number;
	/** With its audio in it (YouTube's `formats`); its size is then the whole file's. */
	muxed?: boolean;
}

/**
 * Heights best first, one entry each. Heights offered only in a codec Windows cannot play are
 * left out when others exist, as `ytdlp::page_info` does (the download takes a playable one).
 */
function offers(formats: Format[], audioSize: number | null): Offer[] {
	const somePlayable = formats.some((f) => playable(f.codec));
	const best = new Map<number, Format>();
	for (const f of formats) {
		if (somePlayable && !playable(f.codec)) continue;
		const b = best.get(f.height);
		// yt-dlp takes a video stream and the best audio; a muxed format only when none is.
		const better = (x: Format, y: Format) =>
			x.muxed !== y.muxed ? !x.muxed : x.fps !== y.fps ? x.fps > y.fps : x.rank > y.rank;
		if (!b || better(f, b)) best.set(f.height, f);
	}
	return [...best.values()]
		.sort((a, b) => b.height - a.height)
		.map((f) => {
			const o: Offer = { height: f.height };
			if (f.fps > 30) o.fps = f.fps;
			if (f.size !== null) o.size = f.size + (f.muxed ? 0 : (audioSize ?? 0));
			return o;
		});
}

const num = (v: unknown): number | null => {
	const n = typeof v === 'string' ? Number(v) : v;
	return typeof n === 'number' && Number.isFinite(n) && n > 0 ? n : null;
};

const obj = (v: unknown): Record<string, unknown> | null =>
	v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null;

// ---- YouTube ----------------------------------------------------------------------------

/** yt-dlp's codec order (`vcodec` in its default format sort): AV1, then VP9, then H.264. */
function youtubeRank(codec: string): number {
	if (codec.startsWith('av01')) return 3;
	if (codec.startsWith('vp9') || codec.startsWith('vp09')) return 2;
	if (codec.startsWith('avc1')) return 1;
	return 0;
}

/** A YouTube player response's formats, or `null` when it is not a playable, finished video. */
export function youtubeFormats(response: unknown): Noted | null {
	const r = obj(response);
	const details = obj(r?.videoDetails);
	const streaming = obj(r?.streamingData);
	const id = details?.videoId;
	if (typeof id !== 'string' || !/^[\w-]{11}$/.test(id) || !streaming) return null;
	const status = obj(r?.playabilityStatus)?.status;
	// A live stream (or one still to come) is read by yt-dlp, which knows what it is.
	if ((status !== undefined && status !== 'OK') || details?.isLive === true || details?.isUpcoming === true) return null;
	const list = [streaming.formats, streaming.adaptiveFormats].flatMap((l) => (Array.isArray(l) ? l : []));
	const video: Format[] = [];
	let audioSize: number | null = null;
	let audio = false;
	for (const item of list) {
		const f = obj(item);
		const mime = typeof f?.mimeType === 'string' ? f.mimeType : '';
		const codecs = mime.match(/codecs="([^"]*)"/)?.[1]?.split(',').map((c) => c.trim()) ?? [];
		const size = num(f?.contentLength);
		const height = num(f?.height);
		if (mime.startsWith('audio/')) {
			audio = true;
			// The largest is about what yt-dlp's `bestaudio` takes.
			if (size !== null && size > (audioSize ?? 0)) audioSize = size;
		} else if (mime.startsWith('video/') && height !== null && codecs[0]) {
			const muxed = codecs.length > 1;
			video.push({ height, fps: num(f?.fps) ?? 0, codec: codecs[0], size, rank: youtubeRank(codecs[0]), muxed });
		}
	}
	if (!video.length) return null;
	return { key: `youtube:${id}`, formats: { heights: offers(video, audioSize), audio } };
}

/** The player responses in a YouTube reply: itself, wrapped, or prefetched in a list of Shorts. */
export function youtubePlayers(body: unknown, max = 50): unknown[] {
	return walk(body, max, (o) => (o.videoDetails && o.streamingData ? o : null));
}

// ---- TikTok -----------------------------------------------------------------------------

/** A TikTok item's formats (`video.bitrateInfo`), or `null`. */
export function tiktokFormats(item: unknown): Noted | null {
	const it = obj(item);
	const id = it?.id;
	const v = obj(it?.video);
	if (typeof id !== 'string' || !/^\d{10,}$/.test(id) || !v || !Array.isArray(v.bitrateInfo)) return null;
	const video: Format[] = [];
	for (const b of v.bitrateInfo) {
		const info = obj(b);
		const play = obj(info?.PlayAddr);
		const height = num(play?.Height);
		if (height === null) continue;
		const key = typeof play?.UrlKey === 'string' ? play.UrlKey : '';
		const codec = typeof info?.CodecType === 'string' ? info.CodecType : key.includes('bytevc1') ? 'bytevc1' : 'h264';
		video.push({ height, fps: num(info?.BitrateFPS) ?? 0, codec, size: num(play?.DataSize), rank: num(info?.Bitrate) ?? 0 });
	}
	if (!video.length) return null;
	return { key: `tiktok:${id}`, formats: { heights: offers(video, null), audio: false } };
}

/** TikTok items anywhere in a feed reply or the page's rehydration data, at most `max`. */
export function tiktokItems(data: unknown, max = 100): Noted[] {
	return walk(data, max, tiktokFormats);
}

/** How many objects a walk of a reply looks at, at most, and how deep. */
const WALK_LIMIT = 20000;
const WALK_DEPTH = 12;

/** What `take` finds in `data`, depth first, at most `max`; what it finds is not looked into. */
function walk<T>(data: unknown, max: number, take: (o: Record<string, unknown>) => T | null): T[] {
	const out: T[] = [];
	let seen = 0;
	const visit = (v: unknown, depth: number) => {
		if (out.length >= max || depth > WALK_DEPTH || ++seen > WALK_LIMIT || !v || typeof v !== 'object') return;
		if (Array.isArray(v)) {
			for (const x of v) visit(x, depth + 1);
			return;
		}
		const found = take(v as Record<string, unknown>);
		if (found) {
			out.push(found);
			return;
		}
		for (const x of Object.values(v)) if (x && typeof x === 'object') visit(x, depth + 1);
	};
	visit(data, 0);
	return out;
}

// ---- Between the page and the button ---------------------------------------------------

/** The key a video's formats are kept under, from its address (address.ts), or `null`. */
export function videoKey(url: string | null): string | null {
	if (!url) return null;
	const yt = url.match(/^https:\/\/www\.youtube\.com\/(?:watch\?v=|shorts\/)([\w-]{11})$/);
	if (yt) return `youtube:${yt[1]}`;
	const tt = url.match(/^https:\/\/www\.tiktok\.com\/@[\w.-]*\/video\/(\d{10,})$/);
	return tt ? `tiktok:${tt[1]}` : null;
}

/** `Noted` as it came through `postMessage` from the page, or `null` when it is not one. */
export function asNoted(v: unknown): Noted | null {
	const n = obj(v);
	const f = obj(n?.formats);
	if (typeof n?.key !== 'string' || !/^(youtube|tiktok):[\w-]{1,32}$/.test(n.key) || !f || !Array.isArray(f.heights)) return null;
	if (f.heights.length > 40) return null;
	const heights: Offer[] = [];
	for (const h of f.heights) {
		const o = obj(h);
		const height = num(o?.height);
		if (height === null || height > 10000) return null;
		const offer: Offer = { height };
		const fps = num(o?.fps);
		const size = num(o?.size);
		if (fps !== null) offer.fps = fps;
		if (size !== null) offer.size = size;
		heights.push(offer);
	}
	return { key: n.key, formats: { heights, audio: f.audio === true } };
}

function bytes(n: number): string {
	if (n >= 1e9) return `${(n / 1e9).toFixed(1)} GB`;
	if (n >= 1e6) return `${Math.round(n / 1e6)} MB`;
	return `${Math.max(1, Math.round(n / 1e3))} KB`;
}

/** The menu's items, with the ids the app takes for a page: a height, or `audio`. */
export function qualitiesOf(f: Formats): Quality[] {
	const out = f.heights.map((o) => {
		const parts = [`${o.height}p`];
		if (o.fps) parts.push(`${Math.round(o.fps)} fps`);
		if (o.size) parts.push(`~${bytes(o.size)}`);
		return { id: String(o.height), label: parts.join(' · ') };
	});
	if (f.audio) out.push({ id: 'audio', label: 'Audio only' });
	return out;
}
