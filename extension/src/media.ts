// Recognising media in a page's network traffic, like cat-catch: which responses are videos,
// audio, or HLS/DASH playlists worth offering, and which are only pieces of them. Pure
// functions, tested with node:test.

export type MediaKind = 'hls' | 'dash' | 'video' | 'audio';

export interface MediaItem {
	url: string;
	kind: MediaKind;
	/** Bytes, when the server said; playlists are small and their size means nothing. */
	size: number | null;
	mime: string | null;
}

/** Plain files smaller than this are thumbnails, previews or ads, not worth listing. */
export const MIN_FILE_BYTES = 512 * 1024;
/** At most this many items per tab; a long-running page keeps the newest. */
export const MAX_ITEMS = 100;

const HLS_TYPES = ['application/vnd.apple.mpegurl', 'application/x-mpegurl', 'audio/mpegurl', 'audio/x-mpegurl'];
const DASH_TYPES = ['application/dash+xml'];
const VIDEO_EXT = ['mp4', 'webm', 'mkv', 'mov', 'flv', 'avi', 'm4v', 'ogv', '3gp'];
const AUDIO_EXT = ['mp3', 'm4a', 'ogg', 'opus', 'wav', 'flac', 'weba'];
/** Pieces of an HLS or DASH stream; the playlist is what to download. */
const SEGMENT_EXT = ['ts', 'm4s', 'cmfv', 'cmfa', 'aac', 'vtt', 'key'];

export interface Response {
	url: string;
	/** `Content-Type`, lowercased, without parameters. */
	contentType: string | null;
	/** `Content-Length` of this response. */
	contentLength: number | null;
	/** `Content-Range` of a 206 response, e.g. `bytes 0-1023/4096`. */
	contentRange: string | null;
}

function extOf(url: URL): string {
	const last = url.pathname.split('/').pop() ?? '';
	const i = last.lastIndexOf('.');
	return i > 0 ? last.slice(i + 1).toLowerCase() : '';
}

/** Total size from a `Content-Range`, or this response's length. */
export function totalSize(r: Response): number | null {
	const total = r.contentRange?.match(/\/(\d+)\s*$/)?.[1];
	if (total) return Number(total);
	return r.contentLength;
}

/** The media this response is, or `null` for anything not worth offering. */
export function classify(r: Response): MediaItem | null {
	let url: URL;
	try {
		url = new URL(r.url);
	} catch {
		return null;
	}
	if (url.protocol !== 'http:' && url.protocol !== 'https:') return null;
	const type = (r.contentType ?? '').split(';')[0].trim().toLowerCase();
	const ext = extOf(url);
	const size = totalSize(r);
	const item = (kind: MediaKind): MediaItem => ({
		url: r.url,
		kind,
		size: kind === 'hls' || kind === 'dash' ? null : size,
		mime: type || null
	});

	if (HLS_TYPES.includes(type) || ext === 'm3u8') return item('hls');
	if (DASH_TYPES.includes(type) || ext === 'mpd') return item('dash');
	if (SEGMENT_EXT.includes(ext) || type === 'video/mp2t' || type === 'video/iso.segment') return null;
	// A range request that does not start at the beginning is a piece of a file already seen
	// (players fetch in chunks); the first chunk lists it.
	if (r.contentRange && !/^bytes\s+0-/i.test(r.contentRange)) return null;

	const isVideo = type.startsWith('video/') || VIDEO_EXT.includes(ext);
	const isAudio = type.startsWith('audio/') || AUDIO_EXT.includes(ext);
	if (!isVideo && !isAudio) return null;
	if (size !== null && size < MIN_FILE_BYTES) return null;
	return item(isVideo ? 'video' : 'audio');
}

/** Adds `item` to `list` once (by URL, ignoring the fragment), newest last, capped. */
export function remember(list: MediaItem[], item: MediaItem): MediaItem[] {
	const key = (u: string) => u.split('#')[0];
	const rest = list.filter((m) => key(m.url) !== key(item.url));
	const next = [...rest, item];
	return next.length > MAX_ITEMS ? next.slice(next.length - MAX_ITEMS) : next;
}

/** A short label for the popup: the file name, or the host for playlists named "index". */
export function label(item: MediaItem): string {
	try {
		const url = new URL(item.url);
		const name = decodeURIComponent(url.pathname.split('/').filter(Boolean).pop() ?? '');
		if (!name || /^(index|master|playlist|manifest)\.(m3u8|mpd)$/i.test(name)) {
			return `${url.hostname} ${item.kind.toUpperCase()}`;
		}
		return name;
	} catch {
		return item.url;
	}
}
