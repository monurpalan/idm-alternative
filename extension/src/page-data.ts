// Runs in the page's own world on YouTube and TikTok (manifest `world: "MAIN"`), as IDM's
// extension does: it notes the formats each video offers from the data the site already
// fetched for its player (see formats.ts), so the menu by a video opens at once. Only a few
// numbers per video are kept; the replies themselves are read once, after the page has them,
// and dropped. The button's script (video-button.ts) asks by `postMessage` for one video at a
// time. Anything that goes wrong here leaves the page as it was: the button then asks the app.

import { tiktokItems, youtubeFormats, youtubePlayers, type Formats, type Noted } from './formats.ts';

/** How many videos are kept, newest last; a feed scrolled for long drops the oldest. */
const KEEP = 300;
/** Replies larger than this are not read. */
const MAX_BODY = 8 * 1024 * 1024;

const youtube = /(^|\.)youtube\.com$/.test(location.hostname);
const wanted = youtube
	? /\/youtubei\/v1\/(player|reel\/reel_item_watch|reel\/reel_watch_sequence)\b/
	: /\/api\/(recommend\/item_list|preload\/item_list|prefetch\/explore\/item_list|related\/item_list|post\/item_list|item\/detail|repost\/item_list|favorite\/item_list|explore\/item_list)\b/;

const noted = new Map<string, Formats>();

function note(found: Noted[]) {
	for (const n of found) {
		noted.delete(n.key);
		noted.set(n.key, n.formats);
	}
	while (noted.size > KEEP) noted.delete(noted.keys().next().value!);
}

function read(data: unknown) {
	try {
		if (youtube) note(youtubePlayers(data).flatMap((p) => youtubeFormats(p) ?? []));
		else note(tiktokItems(data));
	} catch {
		// Not what was expected: the app reads it instead.
	}
}

/** Reads a reply's text later, off the page's own handling of it. */
function readText(text: string) {
	if (text.length > MAX_BODY) return;
	setTimeout(() => {
		try {
			read(JSON.parse(text));
		} catch {
			// Not JSON.
		}
	}, 0);
}

function urlOf(input: unknown): string {
	if (typeof input === 'string') return input;
	if (input instanceof URL) return input.href;
	if (input instanceof Request) return input.url;
	return '';
}

// fetch: the page gets the very same promise; a copy of the reply is read on the side.
const pageFetch = window.fetch;
if (typeof pageFetch === 'function') {
	window.fetch = function (this: unknown, ...args: Parameters<typeof fetch>) {
		const reply = pageFetch.apply(this, args);
		try {
			if (wanted.test(urlOf(args[0]))) {
				reply
					.then((r) => (r.ok ? r.clone().text() : ''))
					.then((t) => t && readText(t))
					.catch(() => {});
			}
		} catch {
			// Never in the page's way.
		}
		return reply;
	} as typeof fetch;
}

// XMLHttpRequest: listens for the end of the requests it wants.
const open = XMLHttpRequest.prototype.open;
XMLHttpRequest.prototype.open = function (this: XMLHttpRequest, ...args: unknown[]) {
	try {
		if (wanted.test(urlOf(args[1]))) {
			this.addEventListener('load', () => {
				try {
					if (this.status < 200 || this.status >= 300) return;
					if (this.responseType === 'json') setTimeout(() => read(this.response), 0);
					else if (this.responseType === '' || this.responseType === 'text') readText(this.responseText);
				} catch {
					// Never in the page's way.
				}
			});
		}
	} catch {
		// As above.
	}
	return (open as (...a: unknown[]) => void).apply(this, args);
} as typeof open;

/** The data the page was served with, read once, when first asked for. */
let servedRead = false;
function readServed() {
	if (servedRead || document.readyState === 'loading') return;
	servedRead = true;
	if (youtube) {
		read((window as unknown as { ytInitialPlayerResponse?: unknown }).ytInitialPlayerResponse);
	} else {
		const el = document.getElementById('__UNIVERSAL_DATA_FOR_REHYDRATION__');
		if (el?.textContent) readText(el.textContent);
	}
}

/** What a page asks and is told; `postMessage` stays within this frame. */
const ASK = 'shardload:formats?';
const TELL = 'shardload:formats';

addEventListener('message', (e) => {
	if (e.source !== window || e.data?.type !== ASK || typeof e.data.key !== 'string') return;
	const answer = () => {
		const formats = noted.get(e.data.key) ?? null;
		window.postMessage({ type: TELL, key: e.data.key, formats }, location.origin);
	};
	if (noted.has(e.data.key) || servedRead) {
		answer();
		return;
	}
	readServed();
	// A TikTok page's served data is read a moment later (see `readText`); the answer follows it.
	setTimeout(answer, 0);
});
