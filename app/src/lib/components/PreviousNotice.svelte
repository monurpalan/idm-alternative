<script lang="ts">
	// "You downloaded this before": what was found for a download about to be added, with
	// the ways to use it instead (open it, show it, go to it on the list).

	import { openFile, openPrevious, showInFolder, showPrevious, type Previous, type TaskId } from '../api.ts';
	import { day } from '../format.ts';
	import { t } from '../i18n.svelte.ts';
	import { Button, Text } from './ui/index.ts';

	let {
		previous,
		onshow,
		onerror
	}: {
		previous: Previous;
		/** Goes to the download on the list (and closes what asked). */
		onshow: (id: TaskId) => void;
		onerror: (message: string) => void;
	} = $props();

	const unfinished = $derived(previous.status !== null && previous.status !== 'done');
	const when = $derived(previous.finished_at !== null ? day(previous.finished_at) : '');

	function run(p: Promise<unknown>) {
		p.catch((e) => onerror(String(e)));
	}

	function open() {
		if (previous.history !== null) run(openPrevious(previous.history));
		else if (previous.id !== null) run(openFile(previous.id));
	}

	function show() {
		if (previous.history !== null) run(showPrevious(previous.history));
		else if (previous.id !== null) run(showInFolder(previous.id));
	}
</script>

<div class="previous" role="status">
	<Text>
		{#if unfinished}
			{t('Already on the list, not finished yet.')}
		{:else if previous.matched === 'name_size'}
			{t('A file with the same name and size was downloaded on {date}.', { date: when })}
		{:else}
			{t('You downloaded this on {date}.', { date: when })}
		{/if}
		{#if !unfinished && !previous.on_disk}
			<Text variant="muted" inline>{t('The file is no longer where it was saved.')}</Text>
		{/if}
	</Text>
	{#if unfinished && previous.id !== null}
		<div class="actions">
			<Button variant="link" onclick={() => onshow(previous.id!)}>{t('Go to it')}</Button>
		</div>
	{:else if previous.on_disk}
		<div class="actions">
			<Button variant="link" onclick={open}>{t('Open')}</Button>
			<Button variant="link" onclick={show}>{t('Show in folder')}</Button>
		</div>
	{/if}
</div>

<style>
	.previous {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		padding: var(--space-2) var(--space-3);
		border-left: 2px solid var(--accent);
		background: var(--panel);
	}
	.actions {
		display: flex;
		gap: var(--space-3);
	}
</style>
