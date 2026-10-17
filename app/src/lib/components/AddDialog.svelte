<script lang="ts">
	// "Add URL": asks the server first, shows name and size, then adds the download.

	import { onMount } from 'svelte';
	import { open as pickFolder } from '@tauri-apps/plugin-dialog';
	import { addTask, getSettings, probe, type Headers, type Login, type Probe } from '../api.ts';
	import { bytes } from '../format.ts';

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
	let checking = $state(false);
	let error = $state<string | null>(null);
	let busy = $state(false);
	/** The URL `info` belongs to; asking again for the same one is pointless. */
	let asked = '';
	/** Credentials for a server that asked for them. */
	let user = $state('');
	let password = $state('');
	let remember = $state(false);
	const login = (): Login | null => (user ? { user, password, remember } : null);
	let dialog: HTMLDialogElement | undefined = $state();

	// Once, on opening; an effect would rerun whenever the fields it reads change.
	onMount(() => {
		url = initialUrl;
		dialog?.showModal();
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
		try {
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
			await addTask({
				url: u,
				// Only send a name the user (or the browser) chose; otherwise the server's
				// choice is made (and made unique) when the download starts.
				name: info && name.trim() && name.trim() !== info.name ? name.trim() : null,
				dir: dir || null,
				start,
				login: login(),
				headers
			});
			onclose();
		} catch (e) {
			error = String(e);
			busy = false;
		}
	}
</script>

<dialog bind:this={dialog} onclose={onclose} aria-labelledby="add-title">
	<form
		onsubmit={(e) => {
			e.preventDefault();
			add(true);
		}}
	>
		<h2 id="add-title">Add download</h2>
		{#if headers?.length}
			<p class="from">From the browser, with its cookies and referrer.</p>
		{/if}

		<label>
			Address
			<input
				type="url"
				bind:value={url}
				onblur={() => check()}
				onkeydown={(e) => {
					if (e.key === 'Enter' && !info) {
						e.preventDefault();
						check();
					}
				}}
				placeholder="https://"
				required
			/>
		</label>

		<div class="info" aria-live="polite">
			{#if checking}
				Asking the server…
			{:else if error}
				<span class="error">{error}</span>
			{:else if info?.needs_login}
				<span class="warn"
					>The server asks for a user name and password{info.realm ? ` (${info.realm})` : ''}.</span
				>
			{:else if info}
				{#if info.saved_login}Logs in as {info.saved_login} (saved for this site) ·{/if}
				{info.size !== null ? bytes(info.size) : 'Size unknown'}
				· {info.resumable ? 'Can resume and use several connections' : 'Cannot resume (one connection)'}
			{:else}
				&nbsp;
			{/if}
		</div>

		{#if info?.needs_login || user}
			<div class="login">
				<label>
					User name
					<input bind:value={user} autocomplete="username" />
				</label>
				<label>
					Password
					<input type="password" bind:value={password} autocomplete="current-password" />
				</label>
				<button type="button" onclick={() => check(true)} disabled={!user || checking}>Sign in</button>
				<label class="remember">
					<input type="checkbox" bind:checked={remember} />
					Remember for this site
				</label>
			</div>
		{/if}

		<label>
			Save as
			<input
				bind:value={name}
				placeholder="Name from the server"
				disabled={!info || info.needs_login}
			/>
		</label>

		<label>
			Folder
			<div class="row">
				<input bind:value={dir} placeholder={defaultDir} />
				<button type="button" onclick={browse}>Browse…</button>
			</div>
		</label>

		<div class="buttons">
			<button type="button" onclick={() => dialog?.close()}>Cancel</button>
			<button type="button" onclick={() => add(false)} disabled={busy || !url.trim()}>
				Download later
			</button>
			<button type="submit" class="primary" disabled={busy || !url.trim()}>
				Start download
			</button>
		</div>
	</form>
</dialog>

<style>
	dialog {
		width: min(560px, calc(100vw - 32px));
		border: 1px solid var(--line);
		border-radius: 10px;
		background: var(--bg);
		color: var(--fg);
		padding: 18px 20px;
		box-shadow: 0 12px 40px rgb(0 0 0 / 0.25);
	}
	dialog::backdrop {
		background: rgb(0 0 0 / 0.3);
	}
	h2 {
		font-size: 16px;
		margin: 0 0 14px;
	}
	label {
		display: flex;
		flex-direction: column;
		gap: 4px;
		margin-bottom: 10px;
		color: var(--muted);
	}
	input {
		padding: 6px 8px;
		border: 1px solid var(--line);
		border-radius: 5px;
		background: var(--field);
		color: var(--fg);
		font: inherit;
		min-width: 0;
	}
	input:disabled {
		opacity: 0.6;
	}
	.row {
		display: flex;
		gap: 6px;
	}
	.row input {
		flex: 1;
	}
	.info {
		margin: -4px 0 12px;
		color: var(--muted);
		min-height: 1.4em;
		overflow-wrap: anywhere;
	}
	.error {
		color: var(--danger);
	}
	.warn {
		color: var(--fg);
	}
	.from {
		margin: -8px 0 12px;
		color: var(--muted);
	}
	.login {
		display: grid;
		grid-template-columns: 1fr 1fr auto;
		align-items: end;
		gap: 8px;
		margin-bottom: 10px;
	}
	.login .remember {
		grid-column: 1 / -1;
		display: flex;
		flex-direction: row;
		align-items: center;
		gap: 6px;
	}
	.login label {
		margin-bottom: 0;
	}
	.buttons {
		display: flex;
		justify-content: flex-end;
		gap: 8px;
		margin-top: 16px;
	}
</style>
