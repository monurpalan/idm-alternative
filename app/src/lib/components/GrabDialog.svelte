<script lang="ts">
	// Site grabber: reads a page and, to the chosen depth, the pages it links to, then hands
	// the files found to the links dialog to pick from.

	import { onDestroy, onMount } from 'svelte';
	import { cancelGrab, grabSite, onGrabProgress, type CapturedLinks } from '../api.ts';
	import { t } from '../i18n.svelte.ts';
	import { Button, Checkbox, Dialog, Field, Input, Select, Text } from './ui/index.ts';

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
	let dialog: { close(): void } | undefined = $state();
	let unlisten: (() => void) | null = null;

	onMount(() => {
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

<Dialog bind:this={dialog} title={t('Site grabber')} {onclose} onsubmit={start}>
	<Field label={t('Start page')}>
		<Input inputmode="url" bind:value={url} placeholder="https://" required disabled={running} />
	</Field>
	<Field label={t('Depth')}>
		<Select bind:value={depth} disabled={running}>
			<option value={1}>{t('This page only')}</option>
			<option value={2}>{t('And the pages it links to')}</option>
			<option value={3}>{t('Two levels of pages below')}</option>
		</Select>
	</Field>
	<Checkbox bind:checked={sameSite} disabled={running}>{t('Stay on this site')}</Checkbox>

	<div aria-live="polite">
		{#if running}
			<Text variant="muted">
				{t('Reading pages: {pages} read, {files} files found', {
					pages: progress?.pages ?? 0,
					files: progress?.files ?? 0
				})}
			</Text>
		{:else if error}
			<Text variant="error">{error}</Text>
		{:else}
			<Text>{' '}</Text>
		{/if}
	</div>

	{#snippet actions()}
		<Button onclick={() => dialog?.close()}>{t('Cancel')}</Button>
		{#if running}
			<Button onclick={stop}>{t('Stop')}</Button>
		{:else}
			<Button type="submit" variant="primary" disabled={!url.trim()}>{t('Find files')}</Button>
		{/if}
	{/snippet}
</Dialog>
