<script lang="ts">
	import { onMount } from 'svelte';
	import {
		addTask,
		listTasks,
		onManagerEvent,
		pauseTask,
		removeTask,
		startTask,
		type Task,
		type TaskId,
		type TaskProgress
	} from '#lib/api.ts';

	// The full list view comes in the next step; this is the skeleton that proves the
	// Rust side, the event stream and the UI are wired together.

	let tasks = $state<Task[]>([]);
	let live = $state<Record<TaskId, TaskProgress>>({});
	let url = $state('');
	let error = $state<string | null>(null);

	async function reload() {
		tasks = await listTasks();
	}

	onMount(() => {
		reload().catch((e) => (error = String(e)));
		const stop = onManagerEvent((e) => {
			switch (e.type) {
				case 'added':
					tasks.push(e.data);
					break;
				case 'updated': {
					const i = tasks.findIndex((t) => t.id === e.data.id);
					if (i >= 0) tasks[i] = e.data;
					if (e.data.status !== 'downloading') delete live[e.data.id];
					break;
				}
				case 'progress':
					live[e.data.id] = e.data;
					break;
				case 'removed':
					tasks = tasks.filter((t) => t.id !== e.data);
					delete live[e.data];
					break;
			}
		}, reload);
		return () => {
			stop.then((f) => f());
		};
	});

	async function add(event: SubmitEvent) {
		event.preventDefault();
		const u = url.trim();
		if (!u) return;
		try {
			await addTask(u);
			url = '';
			error = null;
		} catch (e) {
			error = String(e);
		}
	}

	function bytes(n: number | null): string {
		if (n === null) return '?';
		const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
		let v = n;
		let i = 0;
		while (v >= 1024 && i < units.length - 1) {
			v /= 1024;
			i++;
		}
		return i === 0 ? `${v} B` : `${v.toFixed(1)} ${units[i]}`;
	}

	function percent(t: Task): string {
		const p = live[t.id];
		const size = p?.size ?? t.size;
		const received = p?.received ?? t.received;
		if (!size) return '';
		return `${((received / size) * 100).toFixed(1)}%`;
	}
</script>

<main>
	<form onsubmit={add}>
		<input bind:value={url} placeholder="Paste a link to download" aria-label="Link" />
		<button type="submit">Download</button>
	</form>
	{#if error}<p class="error">{error}</p>{/if}

	<table>
		<thead>
			<tr><th>Name</th><th>Size</th><th>Progress</th><th>Speed</th><th>Status</th><th></th></tr>
		</thead>
		<tbody>
			{#each tasks as t (t.id)}
				<tr>
					<td title={t.url}>{t.name ?? t.url}</td>
					<td>{bytes(live[t.id]?.size ?? t.size)}</td>
					<td>{percent(t)}</td>
					<td>{live[t.id] ? `${bytes(Math.round(live[t.id].speed))}/s` : ''}</td>
					<td title={t.error ?? ''}>{t.status}</td>
					<td class="actions">
						{#if t.status === 'downloading' || t.status === 'queued'}
							<button onclick={() => pauseTask(t.id)}>Pause</button>
						{:else if t.status !== 'done'}
							<button onclick={() => startTask(t.id)}>Resume</button>
						{/if}
						<button onclick={() => removeTask(t.id, false)}>Remove</button>
					</td>
				</tr>
			{:else}
				<tr><td colspan="6" class="empty">No downloads yet</td></tr>
			{/each}
		</tbody>
	</table>
</main>

<style>
	:global(:root) {
		--bg: #ffffff;
		--fg: #1b1b1f;
		--muted: #6b6b76;
		--line: #e3e3e8;
		--accent: #2563eb;
		--danger: #c62828;
		font-family: system-ui, 'Segoe UI', sans-serif;
		font-size: 14px;
	}
	@media (prefers-color-scheme: dark) {
		:global(:root) {
			--bg: #1c1c20;
			--fg: #ececf1;
			--muted: #9a9aa6;
			--line: #33333a;
			--accent: #60a5fa;
			--danger: #ef9a9a;
		}
	}
	:global(body) {
		margin: 0;
		background: var(--bg);
		color: var(--fg);
	}
	main {
		padding: 12px 16px;
	}
	form {
		display: flex;
		gap: 8px;
		margin-bottom: 12px;
	}
	input {
		flex: 1;
		padding: 6px 8px;
		border: 1px solid var(--line);
		border-radius: 4px;
		background: transparent;
		color: inherit;
	}
	button {
		padding: 4px 10px;
		border: 1px solid var(--line);
		border-radius: 4px;
		background: transparent;
		color: inherit;
		cursor: pointer;
	}
	button[type='submit'] {
		background: var(--accent);
		border-color: var(--accent);
		color: white;
	}
	.error {
		color: var(--danger);
	}
	table {
		width: 100%;
		border-collapse: collapse;
	}
	th,
	td {
		text-align: left;
		padding: 6px 8px;
		border-bottom: 1px solid var(--line);
		white-space: nowrap;
	}
	td:first-child {
		max-width: 360px;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	th {
		color: var(--muted);
		font-weight: 500;
	}
	.actions {
		display: flex;
		gap: 4px;
	}
	.empty {
		color: var(--muted);
		text-align: center;
		padding: 32px;
	}
</style>
