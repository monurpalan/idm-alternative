// The toolbar popup: media seen in the current tab, each with a button that hands it to the
// app; and downloads the browser left interrupted, which the app can continue.

import { api } from './api.ts';
import { cookieHeader } from './filter.ts';
import { label, type MediaItem } from './media.ts';
import { hostMissing } from './hello.ts';
import { fileName, rescuable, rescueMessage, type Interrupted } from './rescue.ts';

const HOST = 'io.github.monurpalan.shardload';

function size(bytes: number | null): string {
	if (!bytes) return '';
	const units = ['B', 'KB', 'MB', 'GB'];
	let v = bytes;
	let i = 0;
	while (v >= 1024 && i < units.length - 1) {
		v /= 1024;
		i++;
	}
	return `${v.toFixed(i ? 1 : 0)} ${units[i]}`;
}

async function cookiesFor(url: string): Promise<string | null> {
	try {
		return cookieHeader(await api.cookies.getAll({ url }));
	} catch {
		return null; // no access to the site's cookies: the app tries without them
	}
}

function showError(text: string) {
	const error = document.getElementById('error')!;
	error.textContent = text;
	error.hidden = false;
}

async function send(tab: chrome.tabs.Tab, item: MediaItem) {
	const cookies = await cookiesFor(item.url);
	const reply = (await api.runtime
		.sendNativeMessage(HOST, {
			type: 'media',
			page: tab.url ?? null,
			title: tab.title ?? null,
			items: [item],
			cookies,
			user_agent: navigator.userAgent
		})
		.catch((e: unknown) => ({ ok: false, error: String(e) }))) as { ok: boolean; error?: string };
	if (reply?.ok) {
		window.close();
	} else {
		showError(`The app could not take it: ${reply?.error ?? 'no answer'}`);
	}
}

/** Hands an interrupted download to the app; once it has it, the browser's entry goes. */
async function rescue(item: Interrupted, button: HTMLButtonElement) {
	button.disabled = true;
	button.textContent = 'Handing over…';
	const url = item.finalUrl || item.url;
	const reply = (await api.runtime
		.sendNativeMessage(HOST, rescueMessage(item, await cookiesFor(url), navigator.userAgent))
		.catch((e: unknown) => ({ ok: false, error: String(e) }))) as { ok: boolean; error?: string };
	if (!reply?.ok) {
		button.disabled = false;
		button.textContent = 'Continue in Shardload';
		showError(`The app could not take it: ${reply?.error ?? 'no answer'}`);
		return;
	}
	await api.downloads.erase({ id: item.id }).catch(() => {});
	button.closest('li')?.remove();
	const left = document.getElementById('rescue-list')!.children.length;
	document.getElementById('rescue')!.hidden = left === 0;
}

async function showInterrupted() {
	// Not `state: 'interrupted'` in the query: Chrome leaves out downloads it gave up on after
	// its own retries, which say "interrupted" all the same.
	const recent = (await api.downloads.search({ orderBy: ['-startTime'], limit: 100 })) as Interrupted[];
	const items = recent.filter(rescuable);
	const list = document.getElementById('rescue-list')!;
	document.getElementById('rescue')!.hidden = items.length === 0;
	for (const item of items) {
		const li = document.createElement('li');
		const kind = document.createElement('span');
		kind.className = 'kind';
		kind.textContent = 'STOPPED';
		const name = document.createElement('span');
		name.className = 'name';
		name.title = item.finalUrl || item.url;
		name.textContent = fileName(item.filename);
		const detail = document.createElement('span');
		detail.className = 'size caption';
		detail.textContent = item.totalBytes > 0 ? `${size(item.bytesReceived)} of ${size(item.totalBytes)}` : size(item.bytesReceived);
		name.append(detail);
		const button = document.createElement('button');
		button.textContent = 'Continue in Shardload';
		button.addEventListener('click', () => rescue(item, button));
		li.append(kind, name, button);
		list.append(li);
	}
}

async function show() {
	const [tab] = await api.tabs.query({ active: true, currentWindow: true });
	if (tab?.id === undefined) return;
	const key = `media:${tab.id}`;
	const items = ((await api.storage.session.get(key))[key] as MediaItem[] | undefined) ?? [];
	const list = document.getElementById('list')!;
	document.getElementById('empty')!.hidden = items.length > 0;
	// Newest first: what just started playing is usually what the user wants.
	for (const item of [...items].reverse()) {
		const li = document.createElement('li');
		const kind = document.createElement('span');
		kind.className = 'kind';
		kind.textContent = item.kind.toUpperCase();
		const name = document.createElement('span');
		name.className = 'name';
		name.title = item.url;
		name.textContent = label(item);
		const detail = document.createElement('span');
		detail.className = 'size caption';
		detail.textContent = [size(item.size), item.mime ?? ''].filter(Boolean).join(' · ');
		name.append(detail);
		const button = document.createElement('button');
		button.textContent = 'Download';
		button.addEventListener('click', () => send(tab, item));
		li.append(kind, name, button);
		list.append(li);
	}
}

/**
 * Says when the app is not installed. A hello does not start an app that is installed but not
 * running; only a missing native host means it is not there.
 */
async function checkApp() {
	const reply = (await api.runtime.sendMessage({ hello: true }).catch(() => null)) as {
		ok: boolean;
		error?: string;
	} | null;
	if (reply && !reply.ok && hostMissing(reply.error ?? '')) {
		document.getElementById('missing')!.hidden = false;
	}
}

checkApp();
show();
showInterrupted();
