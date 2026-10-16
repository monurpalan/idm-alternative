// Takes downloads over from the browser and hands them to the app through the native
// messaging host. When the app cannot take one, the browser downloads it after all.

import { DEFAULT_SETTINGS, baseName, cookieHeader, shouldCapture, type Settings } from './filter.ts';

const HOST = 'io.github.monurpalan.idma';
/** Alt held this recently (by any page) means "let the browser download this one". */
const ALT_WINDOW_MS = 1500;

let altDownAt = 0;
/** URLs this extension gave back to the browser; not taken again. */
const givenBack = new Set<string>();

async function settings(): Promise<Settings> {
	const stored = await chrome.storage.sync.get('settings');
	return { ...DEFAULT_SETTINGS, ...(stored.settings ?? {}) };
}

async function cookiesFor(url: string): Promise<string | null> {
	try {
		return cookieHeader(await chrome.cookies.getAll({ url }));
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
		return (await chrome.runtime.sendNativeMessage(HOST, message)) as Reply;
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

chrome.downloads.onCreated.addListener(async (item) => {
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
		await chrome.downloads.cancel(item.id);
		await chrome.downloads.erase({ id: item.id });
	} catch {
		return; // already finished or gone: nothing to take over
	}
	const reply = await handOver(url, item.referrer, baseName(item.filename ?? ''), candidate.size);
	if (!reply.ok) {
		console.warn('idm-alternative: giving the download back to the browser:', reply.error);
		givenBack.add(url);
		await chrome.downloads.download({ url });
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
		const [result] = await chrome.scripting.executeScript({
			target: { tabId: tab.id },
			func: collectLinks
		});
		const links = (result?.result ?? []) as { url: string; text: string }[];
		const reply = await send({ type: 'links', referrer: info.pageUrl ?? tab.url ?? null, links });
		if (!reply.ok) console.warn('idm-alternative: the app could not take the links:', reply.error);
	}
});
