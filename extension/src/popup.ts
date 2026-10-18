// The toolbar popup: media seen in the current tab, each with a button that hands it to the
// app.

import { api } from './api.ts';
import { cookieHeader } from './filter.ts';
import { label, type MediaItem } from './media.ts';

const HOST = 'io.github.monurpalan.idma';

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

async function send(tab: chrome.tabs.Tab, item: MediaItem) {
	let cookies: string | null = null;
	try {
		cookies = cookieHeader(await api.cookies.getAll({ url: item.url }));
	} catch {
		// No access to the site's cookies: the app tries without them.
	}
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
		const error = document.getElementById('error')!;
		error.textContent = `The app could not take it: ${reply?.error ?? 'no answer'}`;
		error.hidden = false;
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

show();
