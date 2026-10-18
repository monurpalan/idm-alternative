<script lang="ts">
	// "Add URL": asks the server first, shows name and size, then adds the download.

	import { onMount } from 'svelte';
	import { open as pickFolder } from '@tauri-apps/plugin-dialog';
	import {
		addBatch,
		addTask,
		batchPreview,
		getSettings,
		inspectTorrent,
		isTorrent,
		probe,
		type Batch,
		type Headers,
		type Login,
		type Probe,
		type TorrentInfo
	} from '../api.ts';
	import { bytes } from '../format.ts';
	import { t } from '../i18n.svelte.ts';
	import { Button, Checkbox, Dialog, Field, Input, Textarea } from './ui/index.ts';

	let {
		url: initialUrl = '',
		name: initialName = null,
		headers = null,
		onclose
	}: {
		url?: string;
		/** The name the browser chose, kept unless the user changes it. */
		name?: string | null;
		/** From the browser: cookies, referrer, user agent. */
		headers?: Headers | null;
		onclose: () => void;
	} = $props();

	let url = $state('');
	let name = $state('');
	/** Empty: the default folder (per type, when the settings say so). */
	let dir = $state('');
	let defaultDir = $state('');
	let info = $state<Probe | null>(null);
	/** The address has ranges (`photo[001-100].jpg`): it stands for this many downloads. */
	let batch = $state<Batch | null>(null);
	/** A magnet link or `.torrent` file: its files, and which of them to download. */
	let torrent = $state<TorrentInfo | null>(null);
	let chosen = $state<boolean[]>([]);
	const chosenSize = $derived(
		torrent ? torrent.files.reduce((sum, f, i) => (chosen[i] ? sum + f.size : sum), 0) : 0
	);
	let checking = $state(false);
	let error = $state<string | null>(null);
	let busy = $state(false);
	/** The URL `info` belongs to; asking again for the same one is pointless. */
	let asked = '';
	/** Other addresses of the same file, one per line. */
	let mirrors = $state('');
	let showMirrors = $state(false);
	const mirrorList = () =>
		mirrors
			.split(/\r?\n/)
			.map((m) => m.trim())
			.filter(Boolean);
	/** Credentials for a server that asked for them. */
	let user = $state('');
	let password = $state('');
	let remember = $state(false);
	const login = (): Login | null => (user ? { user, password, remember } : null);
	let dialog: { close(): void } | undefined = $state();

	// Once, on opening; an effect would rerun whenever the fields it reads change.
	onMount(() => {
		url = initialUrl;
		getSettings()
			.then((s) => (defaultDir = s.per_category ? `${s.dir}\\<type>` : s.dir))
			.catch(() => {});
		if (initialUrl) check();
	});

	async function check(force = false) {
		const u = url.trim();
		if (!u || (u === asked && !force)) return;
		asked = u;
		checking = true;
		error = null;
		info = null;
		batch = null;
		torrent = null;
		try {
			if (isTorrent(u)) {
				const tor = await inspectTorrent(u);
				if (asked !== u) return;
				torrent = tor;
				chosen = tor.files.map(() => true);
				name = tor.name;
				return;
			}
			const b = await batchPreview(u);
			if (asked !== u) return;
			if (b) {
				// Each file is asked about when its download starts.
				batch = b;
				return;
			}
			const p = await probe(u, login(), headers);
			if (asked !== u) return; // the URL changed meanwhile
			info = p;
			if (!p.needs_login) name = initialName || p.name;
		} catch (e) {
			if (asked === u) error = String(e);
		} finally {
			if (asked === u) checking = false;
		}
	}

	async function browse() {
		const picked = await pickFolder({ directory: true, defaultPath: dir || undefined });
		if (typeof picked === 'string') dir = picked;
	}

	async function add(start: boolean) {
		const u = url.trim();
		if (!u) return;
		busy = true;
		try {
			if (batch) {
				await addBatch({ url: u, dir: dir || null, start, login: login(), headers });
				onclose();
				return;
			}
			if (torrent || isTorrent(u)) {
				const all = !torrent || chosen.every((c) => c);
				await addTask({
					url: u,
					name: null,
					dir: dir || null,
					start,
					login: null,
					stream: {
						audio: null,
						torrent: {
							files: all ? null : chosen.flatMap((c, i) => (c ? [i] : []))
						}
					}
				});
				onclose();
				return;
			}
			await addTask({
				url: u,
				// Only send a name the user (or the browser) chose; otherwise the server's
				// choice is made (and made unique) when the download starts.
				name: info && name.trim() && name.trim() !== info.name ? name.trim() : null,
				dir: dir || null,
				start,
				login: login(),
				headers,
				mirrors: mirrorList()
			});
			onclose();
		} catch (e) {
			error = String(e);
			busy = false;
		}
	}
</script>

<Dialog bind:this={dialog} title={t('New download')} {onclose} onsubmit={() => add(true)}>
	{#if headers?.length}
		<p class="muted">{t('From the browser, with its cookies and referrer.')}</p>
	{/if}

	<Field label={t('Link')}>
		<Input
			inputmode="url"
			bind:value={url}
			onblur={() => check()}
			onkeydown={(e) => {
				if (e.key === 'Enter' && !info) {
					e.preventDefault();
					check();
				}
			}}
			placeholder={t('https://…   (photo[001-100].jpg adds a numbered batch)')}
			required
		/>
	</Field>

	<p class="info" aria-live="polite">
		{#if checking && isTorrent(url)}
			{t('Reading the torrent… (a magnet link asks peers, which can take a minute)')}
		{:else if checking}
			{t('Asking the server…')}
		{:else if error}
			<span class="error">{error}</span>
		{:else if torrent}
			{t('Torrent: {n} files, {size}', { n: torrent.files.length, size: bytes(torrent.size) })}
		{:else if batch}
			{t('Batch: {n} downloads', { n: batch.count })} ·
			<span title={batch.first}>{batch.first.split('/').pop()}</span>
			–
			<span title={batch.last}>{batch.last.split('/').pop()}</span>
		{:else if info?.needs_login}
			<span class="strong"
				>{info.realm
					? t('The server asks for a user name and password ({realm}).', { realm: info.realm })
					: t('The server asks for a user name and password.')}</span
			>
		{:else if info}
			{#if info.saved_login}{t('Logs in as {user} (saved for this site)', { user: info.saved_login })} ·{/if}
			{info.size !== null ? bytes(info.size) : t('Size unknown')}
			· {info.resumable ? t('Can resume and use several connections') : t('Cannot resume (one connection)')}
		{:else}
			&nbsp;
		{/if}
	</p>

	{#if torrent && torrent.files.length > 1}
		<fieldset class="files">
			<legend>
				{t('Files ({size} chosen)', { size: bytes(chosenSize) })}
				<Button variant="link" onclick={() => (chosen = chosen.map(() => true))}>{t('All')}</Button>
				<Button variant="link" onclick={() => (chosen = chosen.map(() => false))}>{t('None')}</Button>
			</legend>
			<ul>
				{#each torrent.files as f, i (i)}
					<li>
						<Checkbox bind:checked={chosen[i]}>
							<span class="file">
								<span class="fname" title={f.name}>{f.name}</span>
								<span class="muted">{bytes(f.size)}</span>
							</span>
						</Checkbox>
					</li>
				{/each}
			</ul>
		</fieldset>
	{/if}

	{#if info?.needs_login || user}
		<div class="login">
			<Field label={t('User name')}>
				<Input bind:value={user} autocomplete="username" />
			</Field>
			<Field label={t('Password')}>
				<Input type="password" bind:value={password} autocomplete="current-password" />
			</Field>
			<Button onclick={() => check(true)} disabled={!user || checking}>{t('Sign in')}</Button>
			<div class="wide">
				<Checkbox bind:checked={remember}>{t('Remember for this site')}</Checkbox>
			</div>
		</div>
	{/if}

	<Field label={t('Save as')}>
		<Input
			bind:value={name}
			placeholder={batch ? t('Each from its server') : t('Name from the server')}
			disabled={!info || info.needs_login || !!torrent}
		/>
	</Field>

	{#if !batch && !torrent}
		{#if showMirrors || mirrors}
			<Field label={t('Mirrors: other links to the same file, one per line')}>
				<Textarea bind:value={mirrors} rows={3} placeholder={t('https://mirror.example.org/file.iso')} />
			</Field>
		{:else}
			<div>
				<Button variant="link" onclick={() => (showMirrors = true)}>{t('Add mirror links…')}</Button>
			</div>
		{/if}
	{/if}

	<Field label={t('Folder')} layout="row">
		<Input bind:value={dir} placeholder={defaultDir} />
		<Button onclick={browse}>{t('Browse…')}</Button>
	</Field>

	{#snippet actions()}
		<Button onclick={() => dialog?.close()}>{t('Cancel')}</Button>
		<Button onclick={() => add(false)} disabled={busy || !url.trim()}>{t('Download later')}</Button>
		<Button
			type="submit"
			variant="primary"
			disabled={busy || !url.trim() || (!!torrent && !chosen.some((c) => c))}
		>
			{batch ? t('Start {n} downloads', { n: batch.count }) : t('Start download')}
		</Button>
	{/snippet}
</Dialog>

<style>
	.muted {
		color: var(--muted);
	}
	.info {
		min-height: var(--space-4);
		color: var(--muted);
		overflow-wrap: anywhere;
	}
	.error {
		color: var(--accent);
	}
	.strong {
		color: var(--fg);
	}
	.login {
		display: grid;
		grid-template-columns: 1fr 1fr auto;
		align-items: end;
		gap: var(--space-2);
	}
	.wide {
		grid-column: 1 / -1;
	}
	.files {
		margin: 0;
		padding: var(--space-1) var(--space-2) var(--space-2);
		border: 1px solid var(--line);
		border-radius: var(--radius-1);
	}
	.files legend {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		padding: 0 var(--space-1);
		color: var(--muted);
	}
	.files ul {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		max-height: 176px;
		margin: 0;
		padding: 0;
		list-style: none;
		overflow-y: auto;
	}
	.file {
		display: flex;
		gap: var(--space-2);
		min-width: 0;
	}
	.fname {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
</style>
