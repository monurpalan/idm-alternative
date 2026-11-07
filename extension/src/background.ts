// Takes downloads over from the browser and hands them to the app through the native
// messaging host. When the app cannot take one, the browser downloads it after all.

import {
	DEFAULT_SETTINGS,
	baseName,
	cookieHeader,
	notePost,
	shouldCapture,
	wasPosted,
	type Settings
} from './filter.ts';
import { api } from './api.ts';
import { chooseMedia, classify, remember, type MediaItem } from './media.ts';
import { videoAddress, type Hints } from './address.ts';
import { browserName, helloDue, helloMessage, type HelloTimes } from './hello.ts';

/** Set by the build: which browser family this copy is for. */
declare const TARGET: 'chromium' | 'firefox';

const HOST = 'io.github.monurpalan.shardload';
/** Alt held this recently (by any page) means "let the browser download this one". */
const ALT_WINDOW_MS = 1500;

let altDownAt = 0;
/** URLs this extension gave back to the browser; not taken again. */
const givenBack = new Set<string>();
/**
 * Addresses pages sent a form to (POST), with when. The app only gets the URL and cookies, so
 * it would fetch these with a GET and get something else; the browser keeps them. Each hop of
 * a redirect is noted with its own method, so a POST answered by a redirect to a plain
 * address is still taken over.
 */
const posted = new Map<string, number>();

api.webRequest.onBeforeRequest.addListener(
	(d) => {
		if (d.method === 'POST') notePost(posted, d.url, Date.now());
	},
	{ urls: ['<all_urls>'], types: ['main_frame', 'sub_frame', 'other'] }
);

async function settings(): Promise<Settings> {
	const stored = await api.storage.sync.get('settings');
	return { ...DEFAULT_SETTINGS, ...(stored.settings ?? {}) };
}

async function cookiesFor(url: string): Promise<string | null> {
	try {
		return cookieHeader(await api.cookies.getAll({ url }));
	} catch {
		return null;
	}
}

interface Reply {
	ok: boolean;
	error?: string;
	data?: unknown;
}

async function send(message: object): Promise<Reply> {
	try {
		return (await api.runtime.sendNativeMessage(HOST, message)) as Reply;
	} catch (e) {
		// No host registered, or it failed to start.
		return { ok: false, error: String(e) };
	}
}

/** An open tab showing `page`, if there is one. */
async function tabOf(page: string | undefined): Promise<chrome.tabs.Tab | null> {
	if (!page) return null;
	try {
		return (await api.tabs.query({})).find((t) => t.url === page) ?? null;
	} catch {
		return null;
	}
}

/** The checksum the page shows next to the link to `urls`, as `algorithm:hex`. */
async function hashOnPage(tab: chrome.tabs.Tab | null, urls: string[]): Promise<string | null> {
	if (tab?.id === undefined) return null;
	const ask = api.tabs.sendMessage(tab.id, { hashFor: urls }) as Promise<string | null | undefined>;
	const late = new Promise<null>((r) => setTimeout(() => r(null), 1000));
	try {
		const found = await Promise.race([ask, late]);
		return typeof found === 'string' ? found : null;
	} catch {
		return null; // no frame has the link (or the page has no content script)
	}
}

async function handOver(url: string, referrer: string | undefined, filename?: string, size?: number, links: string[] = []) {
	const tab = await tabOf(referrer);
	return send({
		type: 'download',
		url,
		referrer: referrer || null,
		filename: filename || null,
		size: size && size > 0 ? size : null,
		title: tab?.title || null,
		checksum: await hashOnPage(tab, [...new Set([url, ...links])]),
		cookies: await cookiesFor(url),
		user_agent: navigator.userAgent
	});
}

api.downloads.onCreated.addListener(async (item) => {
	const url = item.finalUrl || item.url;
	const s = await settings();
	const candidate = {
		url,
		filename: item.filename ?? '',
		size: item.totalBytes ?? item.fileSize ?? -1,
		ours: givenBack.delete(url) || item.byExtensionId === chrome.runtime.id,
		posted: wasPosted(posted, url, Date.now())
	};
	if (!shouldCapture(candidate, s, Date.now() - altDownAt < ALT_WINDOW_MS)) return;

	// Stop the browser's copy first, so the file is not fetched twice.
	try {
		await api.downloads.cancel(item.id);
	} catch {
		return; // already finished or gone: nothing to take over
	}
	// Only tidies the browser's list; Firefox may refuse right after a cancel.
	api.downloads.erase({ id: item.id }).catch(() => {});
	let reply: Reply;
	try {
		reply = await handOver(url, item.referrer, baseName(item.filename ?? ''), candidate.size, [item.url]);
	} catch (e) {
		reply = { ok: false, error: String(e) };
	}
	if (!reply?.ok) {
		// Never lose a download: the browser gets it back.
		console.warn('Shardload: giving the download back to the browser:', reply?.error);
		givenBack.add(url);
		await api.downloads.download({ url });
	}
});

// Messages from content scripts (Alt key presses, the button over videos) and from the popup
// (is the app there?).
chrome.runtime.onMessage.addListener(
	(
		message: {
			alt?: boolean;
			videoButton?: { src: string; hints?: Hints; choice?: string | null; items?: MediaItem[] };
			videoQualities?: { src: string; hints?: Hints; ahead?: boolean };
			playlist?: { url: string; kind: string; master?: boolean };
			hello?: boolean;
		},
		sender: chrome.runtime.MessageSender,
		sendResponse: (reply: object | null) => void
	) => {
		if (message?.alt) altDownAt = Date.now();
		// A playlist a page's player fetched, known by its text (sniff.ts).
		const p = message?.playlist;
		if (p && sender.tab?.id !== undefined && /^https?:/i.test(p.url) && (p.kind === 'hls' || p.kind === 'dash')) {
			const item: MediaItem = { url: p.url, kind: p.kind, size: null, mime: null, master: p.master === true };
			updateMedia(sender.tab.id, (list) => remember(list, item));
			return false;
		}
		if (message?.hello) {
			hello(true).then(sendResponse);
			return true;
		}
		if (message?.videoQualities && sender.tab?.id !== undefined) {
			const { src, hints, ahead } = message.videoQualities;
			videoQualities(sender.tab, src, hints, ahead === true).then(sendResponse);
			return true; // the reply comes later
		}
		if (message?.videoButton && sender.tab?.id !== undefined) {
			const { src, hints, choice, items } = message.videoButton;
			sendVideo(sender.tab, src, hints, choice ?? null, items).then(sendResponse);
			return true;
		}
		return false;
	}
);

/** What the button by a video stands for, with the page's cookies, as the app takes it. */
/** Items the qualities were read for, given back with the choice; only web addresses. */
function givenItems(items: MediaItem[] | undefined): MediaItem[] | null {
	if (!Array.isArray(items) || !items.length) return null;
	const ok = items.every((i) => typeof i?.url === 'string' && /^https?:/i.test(i.url) && typeof i.kind === 'string');
	return ok ? items : null;
}

async function videoCapture(tab: chrome.tabs.Tab, src: string, hints: Hints | undefined, given?: MediaItem[]) {
	const key = mediaKey(tab.id!);
	const seen = ((await api.storage.session.get(key))[key] as MediaItem[] | undefined) ?? [];
	// The page's address as the page says it now: feeds change it as they scroll.
	const page = hints?.page || tab.url || null;
	// With the choice, the very items its qualities were read for: the page around the video
	// may have changed since (a preview gone, a feed scrolled), and the app keeps what it read
	// by their address.
	const items = givenItems(given) ?? chooseMedia(src, seen, page, hints && videoAddress(hints));
	if (!items.length) return null;
	return {
		page,
		title: tab.title ?? null,
		items,
		cookies: await cookiesFor(items[0].url),
		user_agent: navigator.userAgent
	};
}

const NO_MEDIA = 'no address found for this video; let it play, or open it';

/**
 * The qualities to list by the video. None when there is nothing to pick, and also when the
 * app cannot list them (an older app, or yt-dlp not set up yet): the video is then sent as
 * it is and the app asks.
 */
async function videoQualities(tab: chrome.tabs.Tab, src: string, hints: Hints | undefined, ahead: boolean) {
	const capture = await videoCapture(tab, src, hints);
	if (!capture) return { ok: false, error: NO_MEDIA };
	let items = capture.items;
	// With the post the video is in first (see `chooseMedia`), the app reads it and the media
	// the tab fetched at the same time, and answers for the post when yt-dlp knows its site.
	const reply = await send({ type: 'media_qualities', ...capture, items, ahead });
	// Asked ahead of a click, an app that is not running says no: nothing is known yet.
	if (ahead && !reply.ok) return { ok: false, error: reply.error };
	const data = reply.ok ? (reply.data as { qualities?: unknown; url?: unknown } | undefined) : undefined;
	// The app answers for the stream with a picture among those sent (not an audio track).
	const chosen = typeof data?.url === 'string' ? items.findIndex((i) => i.url === data.url) : -1;
	// The post: alone. A stream: first, and no post before it (yt-dlp did not know the site).
	if (items[chosen]?.kind === 'page') items = [items[chosen]];
	else if (chosen >= 0) items = [items[chosen], ...items.filter((m, i) => i !== chosen && m.kind !== 'page')];
	const qualities = data?.qualities;
	return { ok: true, qualities: Array.isArray(qualities) ? qualities : [], items };
}

/** Sends what the button by a video stands for, with the quality picked on the page. */
async function sendVideo(
	tab: chrome.tabs.Tab,
	src: string,
	hints: Hints | undefined,
	choice: string | null,
	items?: MediaItem[]
): Promise<Reply> {
	const capture = await videoCapture(tab, src, hints, items);
	if (!capture) return { ok: false, error: NO_MEDIA };
	return send({ type: 'media', ...capture, choice });
}

// Right-click menu.
const MENU_LINK = 'shardload-link';
const MENU_PAGE = 'shardload-page';

// Hello to the app: when the browser starts, when the extension is installed or updated, and
// when the background wakes up later on (at most once an hour). An app that is not running is
// left alone; the next wake-up tries again.

const HELLO_KEY = 'hello';

/** Says hello when one is due (or `fresh`); the app's reply, or `null` when none was due. */
async function sayHello(fresh: boolean): Promise<Reply | null> {
	if (fresh) await api.storage.session.remove(HELLO_KEY);
	const stored = await api.storage.session.get(HELLO_KEY);
	const last = (stored[HELLO_KEY] as HelloTimes | undefined) ?? {};
	const now = Date.now();
	if (!helloDue(last, now)) return null;
	await api.storage.session.set({ [HELLO_KEY]: { ...last, tried: now } });
	const data = navigator as Navigator & {
		userAgentData?: { brands: { brand: string }[] };
		brave?: unknown;
	};
	const browser = browserName({
		firefox: TARGET === 'firefox',
		brands: data.userAgentData?.brands.map((b) => b.brand) ?? [],
		brave: data.brave !== undefined,
		userAgent: navigator.userAgent
	});
	const reply = await send(helloMessage(api.runtime.getManifest().version, browser));
	if (reply.ok) await api.storage.session.set({ [HELLO_KEY]: { ok: now, tried: now } });
	return reply;
}

/** One hello at a time, so the events arriving together at start send one. */
let helloQueue: Promise<Reply | null> = Promise.resolve(null);
function hello(fresh = false): Promise<Reply | null> {
	helloQueue = helloQueue
		.then(() => sayHello(fresh))
		.catch((e) => {
			console.warn('Shardload: hello:', e);
			return null;
		});
	return helloQueue;
}
chrome.runtime.onStartup.addListener(() => hello());
hello();

chrome.runtime.onInstalled.addListener(() => {
	// Installed or updated: the app learns the new version now.
	hello(true);
	chrome.contextMenus.removeAll(() => {
		chrome.contextMenus.create({
			id: MENU_LINK,
			title: 'Download with Shardload',
			contexts: ['link', 'image', 'video', 'audio']
		});
		chrome.contextMenus.create({
			id: MENU_PAGE,
			title: 'Download all links with Shardload',
			contexts: ['page']
		});
	});
});

/** Runs in the page: every link and media source, with its text. */
function collectLinks(): { url: string; text: string }[] {
	const out: { url: string; text: string }[] = [];
	for (const a of Array.from(document.querySelectorAll('a[href]'))) {
		out.push({ url: (a as HTMLAnchorElement).href, text: (a.textContent ?? '').trim().slice(0, 200) });
	}
	for (const m of Array.from(document.querySelectorAll('img[src], video[src], audio[src], source[src]'))) {
		out.push({ url: (m as HTMLImageElement).src, text: (m.getAttribute('alt') ?? '').trim() });
	}
	return out;
}

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
	if (info.menuItemId === MENU_LINK) {
		const url = info.linkUrl || info.srcUrl;
		if (!url) return;
		const reply = await handOver(url, info.pageUrl);
		if (!reply.ok) console.warn('Shardload: the app could not take the link:', reply.error);
	} else if (info.menuItemId === MENU_PAGE && tab?.id !== undefined) {
		const [result] = await api.scripting.executeScript({
			target: { tabId: tab.id },
			func: collectLinks
		});
		const links = (result?.result ?? []) as { url: string; text: string }[];
		const reply = await send({ type: 'links', referrer: info.pageUrl ?? tab.url ?? null, links });
		if (!reply.ok) console.warn('Shardload: the app could not take the links:', reply.error);
	}
});

// Media in each tab's traffic, for the popup (and later the button over videos). Kept in
// session storage: a Manifest V3 background can be stopped between events.

const mediaKey = (tabId: number) => `media:${tabId}`;
/** Updates run one after another, so two responses arriving together do not lose one. */
let mediaQueue: Promise<unknown> = Promise.resolve();

function updateMedia(tabId: number, change: (list: MediaItem[]) => MediaItem[]) {
	mediaQueue = mediaQueue.then(async () => {
		const key = mediaKey(tabId);
		const stored = await api.storage.session.get(key);
		const list = change((stored[key] as MediaItem[] | undefined) ?? []);
		if (list.length) await api.storage.session.set({ [key]: list });
		else await api.storage.session.remove(key);
		await api.action.setBadgeText({ tabId, text: list.length ? String(list.length) : '' }).catch(() => {});
	}).catch((e) => console.warn('Shardload: media list:', e));
}

api.action.setBadgeBackgroundColor({ color: '#2563eb' }).catch(() => {});

api.webRequest.onHeadersReceived.addListener(
	(d) => {
		if (d.tabId < 0 || d.statusCode >= 400) return;
		const header = (name: string) =>
			d.responseHeaders?.find((h) => h.name.toLowerCase() === name)?.value ?? null;
		const length = header('content-length');
		const item = classify({
			url: d.url,
			contentType: header('content-type'),
			contentLength: length !== null && /^\d+$/.test(length) ? Number(length) : null,
			contentRange: header('content-range')
		});
		if (item) updateMedia(d.tabId, (list) => remember(list, item));
	},
	{ urls: ['<all_urls>'], types: ['media', 'xmlhttprequest', 'object', 'other'] },
	['responseHeaders']
);

// A new page starts a new list.
api.tabs.onUpdated.addListener((tabId, change) => {
	if (change.status === 'loading' && change.url) updateMedia(tabId, () => []);
});
api.tabs.onRemoved.addListener((tabId) => updateMedia(tabId, () => []));
