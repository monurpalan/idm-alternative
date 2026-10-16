// The extension's options page: which downloads go to the app.

import { api } from './api.ts';
import { DEFAULT_SETTINGS, parseList, type Settings } from './filter.ts';

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

async function load() {
	const stored = await api.storage.sync.get('settings');
	const s: Settings = { ...DEFAULT_SETTINGS, ...(stored.settings ?? {}) };
	$<HTMLInputElement>('enabled').checked = s.enabled;
	$<HTMLInputElement>('min').value = s.minSizeKb ? String(s.minSizeKb) : '';
	$<HTMLTextAreaElement>('extensions').value = s.extensions.join(' ');
	$<HTMLTextAreaElement>('excluded').value = s.excludedSites.join('\n');
	const reply = await api.runtime
		.sendNativeMessage('io.github.monurpalan.idma', { type: 'ping' })
		.catch((e: unknown) => ({ ok: false, error: String(e) }));
	$('status').textContent = reply?.ok
		? `Connected to idm-alternative ${reply.version ?? ''}`
		: `Cannot reach the app: ${reply?.error ?? 'no answer'}`;
	$('status').className = reply?.ok ? 'ok' : 'bad';
}

$('form').addEventListener('submit', async (e) => {
	e.preventDefault();
	const min = Number($<HTMLInputElement>('min').value.trim() || '0');
	const settings: Settings = {
		enabled: $<HTMLInputElement>('enabled').checked,
		minSizeKb: Number.isFinite(min) && min > 0 ? Math.round(min) : 0,
		extensions: parseList($<HTMLTextAreaElement>('extensions').value),
		excludedSites: parseList($<HTMLTextAreaElement>('excluded').value)
	};
	await api.storage.sync.set({ settings });
	$('saved').textContent = 'Saved';
	setTimeout(() => ($('saved').textContent = ''), 1500);
});

$('reset').addEventListener('click', async () => {
	await api.storage.sync.remove('settings');
	await load();
});

load();
