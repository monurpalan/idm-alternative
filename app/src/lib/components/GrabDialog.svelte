<script lang="ts">
	// Site grabber: reads a page and, to the chosen depth, the pages it links to, then hands
	// the files found to the links dialog to pick from.

	import { onDestroy, onMount } from 'svelte';
	import { cancelGrab, grabSite, onGrabProgress, type CapturedLinks } from '../api.ts';
	import { t } from '../i18n.svelte.ts';

	let {
		onfound,
		onclose
	}: {
		onfound: (links: CapturedLinks) => void;
		onclose: () => void;
	} = $props();

	let url = $state('');
	let depth = $state(1);
	let sameSite = $state(true);
	let running = $state(false);
	let progress = $state<{ pages: number; files: number } | null>(null);
	let error = $state<string | null>(null);
	let dialog: HTMLDialogElement | undefined = $state();
	let unlisten: (() => void) | null = null;

	onMount(() => {
		dialog?.showModal();
		onGrabProgress((p) => (progress = p)).then((u) => (unlisten = u));
	});
	onDestroy(() => {
		unlisten?.();
		if (running) cancelGrab().catch(() => {});
	});

	async function start() {
		const u = url.trim();
		if (!u) return;
		running = true;
		error = null;
		progress = null;
		try {
			const files = await grabSite(u, depth, sameSite);
			if (!running) return; // stopped meanwhile
			if (!files.length) {
				error = t('No files found on these pages.');
				return;
			}
			onfound({ referrer: u, links: files.map((f) => ({ url: f.url, text: f.text })) });
		} catch (e) {
			if (running) error = String(e);
		} finally {
			running = false;
		}
	}

	function stop() {
		running = false;
		cancelGrab().catch(() => {});
	}
</script>

<dialog bind:this={dialog} onclose={onclose} aria-labelledby="grab-title">
	<form
		onsubmit={(e) => {
			e.preventDefault();
			start();
		}}
	>
		<h2 id="grab-title">{t('Site grabber')}</h2>
		<label>
			{t('Start page')}
			<input type="text" inputmode="url" bind:value={url} placeholder="https://" required disabled={running} />
		</label>
		<label>
			{t('Depth')}
			<select bind:value={depth} disabled={running}>
				<option value={1}>{t('This page only')}</option>
				<option value={2}>{t('And the pages it links to')}</option>
				<option value={3}>{t('Two levels of pages below')}</option>
			</select>
		</label>
		<label class="check">
			<input type="checkbox" bind:checked={sameSite} disabled={running} />
			{t('Stay on this site')}
		</label>

		<div class="info" aria-live="polite">
			{#if running}
				{t('Reading pages: {pages} read, {files} files found', {
					pages: progress?.pages ?? 0,
					files: progress?.files ?? 0
				})}
			{:else if error}
				<span class="error">{error}</span>
			{:else}
				&nbsp;
			{/if}
		</div>

		<div class="buttons">
			<button type="button" onclick={() => dialog?.close()}>{t('Cancel')}</button>
			{#if running}
				<button type="button" onclick={stop}>{t('Stop')}</button>
			{:else}
				<button type="submit" class="primary" disabled={!url.trim()}>{t('Find files')}</button>
			{/if}
		</div>
	</form>
</dialog>

<style>
	dialog {
		width: min(480px, calc(100vw - 32px));
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
	label.check {
		flex-direction: row;
		align-items: center;
		gap: 6px;
		color: var(--fg);
	}
	input,
	select {
		padding: 6px 8px;
		border: 1px solid var(--line);
		border-radius: 5px;
		background: var(--field);
		color: var(--fg);
		font: inherit;
	}
	.info {
		color: var(--muted);
		min-height: 1.4em;
		overflow-wrap: anywhere;
	}
	.error {
		color: var(--danger);
	}
	.buttons {
		display: flex;
		justify-content: flex-end;
		gap: 8px;
		margin-top: 16px;
	}
</style>
