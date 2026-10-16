// Takes downloads over from the browser and hands them to the app through the native
// messaging host. When the app cannot take one, the browser downloads it after all.

import { DEFAULT_SETTINGS, baseName, cookieHeader, shouldCapture, type Settings } from './filter.ts';
import { api } from './api.ts';
import { classify, remember, type MediaItem } from './media.ts';

const HOST = 'io.github.monurpalan.idma';
/** Alt held this recently (by any page) means "let the browser download this one". */
const ALT_WINDOW_MS = 1500;

let altDownAt = 0;
/** URLs this extension gave back to the browser; not taken again. */
const givenBack = new Set<string>();

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
}

async function send(message: object): Promise<Reply> {
	try {
		return (await api.runtime.sendNativeMessage(HOST, message)) as Reply;
	} catch (e) {
		// No host registered, or it failed to start.
		return { ok: false, error: String(e) };
	}
}

async function handOver(url: string, referrer: string | undefined, filename?: string, size?: number) {
	return send({
		type: 'download',
		url,
		referrer: referrer || null,
		filename: filename || null,
		size: size && size > 0 ? size : null,
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
		ours: givenBack.delete(url) || item.byExtensionId === chrome.runtime.id
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
		reply = await handOver(url, item.referrer, baseName(item.filename ?? ''), candidate.size);
	} catch (e) {
		reply = { ok: false, error: String(e) };
	}
	if (!reply?.ok) {
		// Never lose a download: the browser gets it back.
		console.warn('idm-alternative: giving the download back to the browser:', reply?.error);
		givenBack.add(url);
		await api.downloads.download({ url });
	}
});

// The content script reports Alt key presses.
chrome.runtime.onMessage.addListener((message: { alt?: boolean }) => {
	if (message?.alt) altDownAt = Date.now();
});

// Right-click menu.
const MENU_LINK = 'idma-link';
const MENU_PAGE = 'idma-page';

chrome.runtime.onInstalled.addListener(() => {
	chrome.contextMenus.removeAll(() => {
		chrome.contextMenus.create({
			id: MENU_LINK,
			title: 'Download with idm-alternative',
			contexts: ['link', 'image', 'video', 'audio']
		});
		chrome.contextMenus.create({
			id: MENU_PAGE,
			title: 'Download all links with idm-alternative',
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
		if (!reply.ok) console.warn('idm-alternative: the app could not take the link:', reply.error);
	} else if (info.menuItemId === MENU_PAGE && tab?.id !== undefined) {
		const [result] = await api.scripting.executeScript({
			target: { tabId: tab.id },
			func: collectLinks
		});
		const links = (result?.result ?? []) as { url: string; text: string }[];
		const reply = await send({ type: 'links', referrer: info.pageUrl ?? tab.url ?? null, links });
		if (!reply.ok) console.warn('idm-alternative: the app could not take the links:', reply.error);
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
	}).catch((e) => console.warn('idm-alternative: media list:', e));
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
