<script lang="ts">
	// One download in a small window of its own, like IDM's: progress, speed, time left, and
	// once it is done, Open and Show in folder. Opened per download (see `open_progress`).

	import { onMount } from 'svelte';
	import { getCurrentWindow } from '@tauri-apps/api/window';
	import { openFile, pauseTasks, showInFolder, startTasks, type TaskId } from '#lib/api.ts';
	import { downloads, row } from '#lib/downloads.svelte.ts';
	import { bytes, duration, speed } from '#lib/format.ts';

	const id: TaskId = Number(new URLSearchParams(location.search).get('id'));
	let error = $state<string | null>(null);

	const task = $derived(downloads.tasks.find((t) => t.id === id));
	const r = $derived(task ? row(task, downloads.live[id]) : null);

	onMount(() => downloads.connect());

	// The title follows the name the server gives.
	$effect(() => {
		if (r) getCurrentWindow().setTitle(r.task.status === 'done' ? `Done: ${r.name}` : r.name);
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
</script>

<main>
	{#if !downloads.loaded}
		<p class="muted">Loading…</p>
	{:else if !r}
		<p class="muted">This download is no longer on the list.</p>
		<div class="buttons"><button onclick={close}>Close</button></div>
	{:else}
		<h1 title={r.name}>{r.name}</h1>
		<p class="url" title={r.task.url}>{r.task.url}</p>

		<div
			class="bar"
			class:done={r.task.status === 'done'}
			class:stopped={r.task.status === 'error' || r.task.status === 'paused'}
			role="progressbar"
			aria-valuemin="0"
			aria-valuemax="100"
			aria-valuenow={r.fraction === null ? undefined : Math.round(r.fraction * 100)}
		>
			<span style:width="{(r.fraction ?? 0) * 100}%"></span>
		</div>

		<dl>
			<dt>Status</dt>
			<dd>
				{#if r.task.status === 'downloading'}
					{r.fraction === null ? 'Downloading' : `${(r.fraction * 100).toFixed(1)} %`}
					{#if r.live?.connections}· {r.live.connections} connections{/if}
				{:else if r.task.status === 'error'}
					<span class="error">{r.task.error ?? 'Failed'}</span>
				{:else}
					{({ queued: 'Waiting in the queue', paused: 'Paused', done: 'Complete' } as const)[
						r.task.status
					]}
				{/if}
			</dd>
			<dt>Downloaded</dt>
			<dd>{bytes(r.received)}{r.size !== null ? ` of ${bytes(r.size)}` : ''}</dd>
			{#if r.task.status === 'downloading'}
				<dt>Speed</dt>
				<dd>{speed(r.speed)}</dd>
				<dt>Time left</dt>
				<dd>{duration(r.eta)}</dd>
			{/if}
		</dl>

		{#if error}<p class="error">{error}</p>{/if}

		<div class="buttons">
			{#if r.task.status === 'done'}
				<button onclick={() => run(showInFolder(id))}>Show in folder</button>
				<button class="primary" onclick={() => run(openFile(id)).then(close)}>Open</button>
				<button onclick={close}>Close</button>
			{:else}
				{#if r.task.status === 'downloading' || r.task.status === 'queued'}
					<button onclick={() => run(pauseTasks([id]))}>Pause</button>
				{:else}
					<button onclick={() => run(startTasks([id]))}>Resume</button>
				{/if}
				<button onclick={close} title="The download goes on; the list shows it">Hide</button>
			{/if}
		</div>
	{/if}
</main>

<style>
	main {
		display: flex;
		flex-direction: column;
		gap: 8px;
		height: 100vh;
		box-sizing: border-box;
		padding: 14px 16px;
		background: var(--bg);
		color: var(--fg);
	}
	h1 {
		margin: 0;
		font-size: 14px;
		font-weight: 600;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.url {
		margin: 0;
		font-size: 12px;
		color: var(--muted);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.bar {
		height: 10px;
		border-radius: 5px;
		background: var(--line);
		overflow: hidden;
	}
	.bar span {
		display: block;
		height: 100%;
		background: var(--accent);
	}
	.bar.done span {
		background: var(--ok, var(--accent));
	}
	.bar.stopped span {
		background: var(--muted);
	}
	dl {
		display: grid;
		grid-template-columns: 90px 1fr;
		gap: 2px 10px;
		margin: 0;
		font-size: 13px;
		font-variant-numeric: tabular-nums;
	}
	dt {
		color: var(--muted);
	}
	dd {
		margin: 0;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.buttons {
		display: flex;
		justify-content: flex-end;
		gap: 8px;
		margin-top: auto;
	}
	.error {
		color: var(--danger);
	}
	.muted {
		color: var(--muted);
	}
</style>
