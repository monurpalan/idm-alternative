<script lang="ts">
	// One download in a small window of its own, like IDM's: progress, speed, time left, and
	// once it is done, Open and Show in folder. Opened per download (see `open_progress`).

	import { onMount } from 'svelte';
	import { getCurrentWindow } from '@tauri-apps/api/window';
	import { openFile, pauseTasks, showInFolder, startTasks, type TaskId } from '#lib/api.ts';
	import { downloads, row } from '#lib/downloads.svelte.ts';
	import { bytes, duration, percent, speed } from '#lib/format.ts';
	import { msg, t } from '#lib/i18n.svelte.ts';
	import { Button, Progress, Text } from '#lib/components/ui/index.ts';

	const id: TaskId = Number(new URLSearchParams(location.search).get('id'));
	let error = $state<string | null>(null);

	const task = $derived(downloads.tasks.find((t) => t.id === id));
	const r = $derived(task ? row(task, downloads.live[id]) : null);

	onMount(() => downloads.connect());

	// The title follows the name the server gives.
	$effect(() => {
		if (r) getCurrentWindow().setTitle(r.task.status === 'done' ? t('Done: {name}', { name: r.name }) : r.name);
	});

	async function run(p: Promise<unknown>) {
		try {
			await p;
			error = null;
		} catch (e) {
			error = String(e);
		}
	}

	const close = () => getCurrentWindow().close();

	const STATUS = {
		queued: msg('Waiting in the queue'),
		paused: msg('Paused'),
		done: msg('Done')
	} as const;

	const BAR = { downloading: 'running', queued: 'running', paused: 'stopped', done: 'done', error: 'error' } as const;
</script>

<main>
	{#if !downloads.loaded}
		<Text variant="muted">{t('Loading…')}</Text>
	{:else if !r}
		<Text variant="muted">{t('This download is no longer on the list.')}</Text>
		<div class="buttons"><Button onclick={close}>{t('Close')}</Button></div>
	{:else}
		<div class="head">
			<h1 title={r.name}>{r.name}</h1>
			<Text variant="caption" truncate title={r.task.url}>{r.task.url}</Text>
		</div>

		<div class="readout">
			<span class="big">{r.fraction !== null ? percent(r.fraction) : bytes(r.received)}</span>
			{#if r.task.status === 'downloading'}<span class="rate">{speed(r.speed)}</span>{/if}
		</div>
		<Progress value={r.fraction} state={BAR[r.task.status]} label={t('Status')} />

		<dl>
			<dt>{t('Status')}</dt>
			<dd>
				{#if r.task.status === 'downloading'}
					{t('Downloading')}
					{#if r.live?.connections}· {t('{n} connections', { n: r.live.connections })}{/if}
				{:else if r.task.status === 'error'}
					<span class="error">{r.task.error ?? t('Failed')}</span>
				{:else}
					{t(STATUS[r.task.status])}
				{/if}
			</dd>
			<dt>{t('Downloaded')}</dt>
			<dd>
				{r.size !== null
					? t('{a} of {b}', { a: bytes(r.received), b: bytes(r.size) })
					: bytes(r.received)}
			</dd>
			{#if r.task.status === 'downloading'}
				<dt>{t('Speed')}</dt>
				<dd>{speed(r.speed)}</dd>
				<dt>{t('Time left')}</dt>
				<dd>{duration(r.eta)}</dd>
			{/if}
		</dl>

		{#if error}<Text variant="error">{error}</Text>{/if}

		<div class="buttons">
			{#if r.task.status === 'done'}
				<Button onclick={() => run(showInFolder(id))}>{t('Show in folder')}</Button>
				<Button variant="primary" onclick={() => run(openFile(id)).then(close)}>{t('Open')}</Button>
				<Button onclick={close}>{t('Close')}</Button>
			{:else}
				{#if r.task.status === 'downloading' || r.task.status === 'queued'}
					<Button onclick={() => run(pauseTasks([id]))}>{t('Pause')}</Button>
				{:else}
					<Button onclick={() => run(startTasks([id]))}>{t('Resume')}</Button>
				{/if}
				<Button onclick={close} title={t('The download goes on; the list shows it')}>{t('Hide')}</Button>
			{/if}
		</div>
	{/if}
</main>

<style>
	main {
		border-top: var(--space-1) solid var(--accent);
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		height: 100vh;
		box-sizing: border-box;
		padding: var(--space-4);
		background: var(--bg);
		color: var(--fg);
	}
	.head {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		min-width: 0;
	}
	h1 {
		overflow: hidden;
		font-size: var(--text-h2);
		font-weight: 600;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	dl {
		display: grid;
		grid-template-columns: 96px 1fr;
		gap: var(--space-1) var(--space-3);
		margin: 0;
		font-variant-numeric: tabular-nums;
	}
	dt {
		color: var(--muted);
		font-family: var(--font-mono);
		font-size: var(--text-caption);
		letter-spacing: 0.06em;
		line-height: 19px;
		text-transform: uppercase;
	}
	dd {
		font-family: var(--font-mono);
		min-width: 0;
		margin: 0;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.readout {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		font-family: var(--font-mono);
		font-variant-numeric: tabular-nums;
	}
	.big {
		font-size: var(--text-h1);
		font-weight: 500;
	}
	.rate {
		color: var(--muted);
	}
	.buttons {
		display: flex;
		justify-content: flex-end;
		gap: var(--space-2);
		margin-top: auto;
	}
	.error {
		color: var(--danger);
	}
</style>
