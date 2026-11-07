// The address of the one video the button was pressed by, on sites whose pages show many
// (a YouTube home page or search, a TikTok feed). Their players play from `blob:` sources, so
// the page's address is all there is to hand to yt-dlp, and on a feed that address is the
// feed: yt-dlp then lists everything on it, and the media files the tab fetched belong to
// whichever videos the site loaded ahead. The video's own address is found around it in the
// page instead: YouTube's previews sit inside the link to their video, TikTok names each
// player's box after its video's id. Pure, tested with node:test.

/** What the content script finds around a video, closest first. */
export interface Hints {
	/** The page's address as it is now (sites change it as the feed scrolls). */
	page: string;
	/** Links the video is inside. */
	around: string[];
	/** Links in the boxes around the video, those not much larger than it. */
	near: string[];
	/** Ids of the elements the video is inside. */
	ids: string[];
	/**
	 * Links naming the post the video is in: those holding its time (`<a><time>`, as nearly
	 * every social site shows a post's time as the link to it), and `permalink` attributes
	 * (Reddit), closest first.
	 */
	post?: string[];
}

/**
 * `known`: a site handled here, whose answer is final; `url`: its video's address, `null` when
 * none was found. On other sites `url` is the post's address when one was found (see
 * [`postAddress`]), worth trying with yt-dlp before the media the tab fetched.
 */
export interface Address {
	known: boolean;
	url: string | null;
}

function parse(url: string): URL | null {
	try {
		return new URL(url);
	} catch {
		return null;
	}
}

function site(host: string, name: string): boolean {
	return host === name || host.endsWith(`.${name}`);
}

/** A YouTube video's address in a canonical form, or `null` for any other address. */
export function youtubeVideo(url: string): string | null {
	const u = parse(url);
	if (!u || !(site(u.hostname, 'youtube.com') || u.hostname === 'youtu.be')) return null;
	const id = (s: string | null | undefined) => (s && /^[\w-]{11}$/.test(s) ? s : null);
	if (u.hostname === 'youtu.be') {
		const v = id(u.pathname.slice(1));
		return v && `https://www.youtube.com/watch?v=${v}`;
	}
	if (u.pathname === '/watch') {
		const v = id(u.searchParams.get('v'));
		return v && `https://www.youtube.com/watch?v=${v}`;
	}
	const m = u.pathname.match(/^\/(shorts|live|embed)\/([\w-]{11})(?:\/|$)/);
	if (!m) return null;
	return m[1] === 'shorts' ? `https://www.youtube.com/shorts/${m[2]}` : `https://www.youtube.com/watch?v=${m[2]}`;
}

/** A TikTok video's address, or `null`. */
export function tiktokVideo(url: string): string | null {
	const u = parse(url);
	if (!u || !site(u.hostname, 'tiktok.com')) return null;
	const m = u.pathname.match(/^\/@([\w.-]*)\/video\/(\d+)/);
	return m ? `https://www.tiktok.com/@${m[1]}/video/${m[2]}` : null;
}

/** An Instagram reel's or post's address, or `null`. */
export function instagramVideo(url: string): string | null {
	const u = parse(url);
	if (!u || !site(u.hostname, 'instagram.com')) return null;
	const m = u.pathname.match(/^(?:\/[\w.]+)?\/(reels?|p|tv)\/([\w-]+)/);
	return m ? `https://www.instagram.com/${m[1] === 'p' ? 'p' : 'reel'}/${m[2]}/` : null;
}

export function videoAddress(h: Hints): Address {
	const host = parse(h.page)?.hostname.toLowerCase() ?? '';
	const first = (urls: string[], read: (u: string) => string | null) => {
		for (const u of urls) {
			const found = read(u);
			if (found) return found;
		}
		return null;
	};

	if (site(host, 'youtube.com') || host === 'youtu.be') {
		// A preview inside the link to its video; else the page's own video (the player of a
		// watch page holds links to other videos, at its end, so those come last).
		const url = first(h.around, youtubeVideo) ?? youtubeVideo(h.page) ?? first(h.near, youtubeVideo);
		return { known: true, url };
	}
	if (site(host, 'tiktok.com')) {
		// The player's box is named `xgwrapper-<n>-<video id>`; the author's link is near it.
		const id = h.ids.map((i) => i.match(/^xgwrapper-\d+-(\d{10,})$/)?.[1]).find(Boolean);
		if (id) {
			const author = h.near
				.map((u) => parse(u)?.pathname.match(/^\/@([\w.-]+)\/?$/)?.[1])
				.find(Boolean);
			return { known: true, url: `https://www.tiktok.com/@${author ?? ''}/video/${id}` };
		}
		return { known: true, url: first([...h.around, ...h.near], tiktokVideo) ?? tiktokVideo(h.page) };
	}
	if (site(host, 'instagram.com')) {
		return { known: true, url: first(h.around, instagramVideo) ?? first(h.near, instagramVideo) ?? instagramVideo(h.page) };
	}
	return { known: false, url: postAddress(h) };
}

/** The last two labels of a host (`x.com`, `reddit.com`), enough to tell the same site. */
function base(host: string): string {
	return host.toLowerCase().split('.').slice(-2).join('.');
}

/** Path segments that look like an item's id: a long number, or letters and digits mixed. */
function idLike(segment: string): boolean {
	return /^\d{5,}$/.test(segment) || (/^[\w-]{6,}$/.test(segment) && /\d/.test(segment) && /[a-z]/i.test(segment));
}

/**
 * On any other site, the address of the post the video is in: a link it sits inside, or the
 * post's own link (its time, or a `permalink`), on the same site and naming one item (an id in
 * its path). `/status/<id>` and the like are cut there, so `/status/1/video/1` is the post.
 */
export function postAddress(h: Hints): string | null {
	const page = parse(h.page);
	if (!page) return null;
	const read = (url: string): string | null => {
		const u = parse(url);
		if (!u || (u.protocol !== 'https:' && u.protocol !== 'http:')) return null;
		if (base(u.hostname) !== base(page.hostname)) return null;
		const parts = u.pathname.split('/').filter(Boolean);
		// The id after a first segment (a user, a section), or a lone number (`/123456`): a
		// user name with digits (`/user123/likes`) is not a post.
		const lone = parts.length === 1 && /^\d{5,}$/.test(parts[0]);
		if (!lone && !parts.slice(1).some(idLike)) return null;
		const status = parts.findIndex((p, i) => /^(status|statuses|post|posts|comments|videos?|reel|p)$/i.test(p) && idLike(parts[i + 1] ?? ''));
		const keep = status >= 0 ? parts.slice(0, status + 2) : parts;
		return `${u.origin}/${keep.join('/')}`;
	};
	for (const url of [...h.around, ...(h.post ?? [])]) {
		const found = read(url);
		if (found) return found;
	}
	return null;
}
