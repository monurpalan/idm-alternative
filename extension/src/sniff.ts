// Runs in every page's own world (manifest `world: "MAIN"`), in every frame, as IDM's extension
// does: it spots the playlists a site's player fetches by how their text begins (see
// `playlistKind`), whatever they are named or served as. Many sites hide them from anything
// that looks only at names and types (`/x/1.png` served as `text/plain`), and the button by
// a video then found nothing. Only the first bytes of a copy of each reply are read, after the
// page has it; the page's own handling is never touched, and nothing here can break it.

import { playlistKind } from './media.ts';

/** Replies said to be larger than this are not playlists. */
const MAX_LENGTH = 16 * 1024 * 1024;
/** Enough of the start to see past an XML declaration and a comment before `<MPD`. */
const HEAD = 512;

const told = new Set<string>();

function tell(url: string, head: string) {
	const kind = playlistKind(head);
	if (!kind || told.has(url) || !/^https?:/i.test(url)) return;
	told.add(url);
	// To the extension's script in this frame (content.ts), which hands it to the background.
	window.postMessage({ type: 'shardload:playlist', url, kind }, location.origin);
}

/** Media and fonts are what they say; anything else may be a playlist in disguise. */
function maybe(type: string | null, length: string | null): boolean {
	if (type && /^\s*(video|audio|font|image\/(svg|webp|avif|gif))\b/i.test(type)) return false;
	return !(length && Number(length) > MAX_LENGTH);
}

const decode = (bytes: Uint8Array) => new TextDecoder().decode(bytes.subarray(0, HEAD));

const pageFetch = window.fetch;
if (typeof pageFetch === 'function') {
	window.fetch = function (this: unknown, ...args: Parameters<typeof fetch>) {
		const reply = pageFetch.apply(this, args);
		reply
			.then(async (r) => {
				if (!r.ok || !r.body || !maybe(r.headers.get('content-type'), r.headers.get('content-length'))) return;
				const reader = r.clone().body!.getReader();
				const { value } = await reader.read();
				reader.cancel().catch(() => {});
				if (value) tell(r.url, decode(value));
			})
			.catch(() => {});
		return reply;
	} as typeof fetch;
}

const send = XMLHttpRequest.prototype.send;
XMLHttpRequest.prototype.send = function (this: XMLHttpRequest, ...args: unknown[]) {
	try {
		this.addEventListener('load', () => {
			try {
				if (this.status < 200 || this.status >= 300) return;
				if (!maybe(this.getResponseHeader('content-type'), this.getResponseHeader('content-length'))) return;
				const url = this.responseURL;
				if (this.responseType === '' || this.responseType === 'text') tell(url, this.responseText.slice(0, HEAD));
				else if (this.responseType === 'arraybuffer' && this.response) tell(url, decode(new Uint8Array(this.response)));
				else if (this.responseType === 'blob' && this.response)
					(this.response as Blob).slice(0, HEAD).text().then((t) => tell(url, t), () => {});
			} catch {
				// Never in the page's way.
			}
		});
	} catch {
		// As above.
	}
	return (send as (...a: unknown[]) => void).apply(this, args);
} as typeof send;
