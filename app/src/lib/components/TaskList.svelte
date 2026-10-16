<script lang="ts">
	import type { Task, TaskId } from '../api.ts';
	import { downloads, row, type Row, type SortKey } from '../downloads.svelte.ts';
	import { bytes, date, duration, speed } from '../format.ts';

	// Gets the tasks already filtered and in order; each row reads its own live progress, so a
	// progress update re-renders only the rows of running downloads.
	let {
		tasks,
		sortKey = $bindable(),
		sortDesc = $bindable(),
		oncontext,
		onopen
	}: {
		tasks: Task[];
		sortKey: SortKey;
		sortDesc: boolean;
		oncontext: (e: MouseEvent, task: Task) => void;
		onopen: (task: Task) => void;
	} = $props();

	const COLUMNS: { key: SortKey; label: string; numeric?: boolean }[] = [
		{ key: 'queue', label: '#', numeric: true },
		{ key: 'name', label: 'Name' },
		{ key: 'size', label: 'Size', numeric: true },
		{ key: 'progress', label: 'Progress' },
		{ key: 'speed', label: 'Speed', numeric: true },
		{ key: 'eta', label: 'Time left', numeric: true },
		{ key: 'status', label: 'Status' },
		{ key: 'added', label: 'Added', numeric: true }
	];

	/** 1-based place of each unfinished download in the queue. */
	const queuePos = $derived.by(() => {
		const pos = new Map<TaskId, number>();
		downloads.tasks
			.filter((t) => t.status !== 'done')
			.sort((a, b) => a.queue_pos - b.queue_pos)
			.forEach((t, i) => pos.set(t.id, i + 1));
		return pos;
	});

	/** Where a Shift-click range starts. */
	let anchor: TaskId | null = null;

	function sortBy(key: SortKey) {
		if (sortKey === key) sortDesc = !sortDesc;
		else {
			sortKey = key;
			// Big numbers and recent dates first; names A→Z.
			sortDesc = key !== 'name' && key !== 'status' && key !== 'queue';
		}
	}

	function select(e: MouseEvent, t: Task) {
		const sel = downloads.selected;
		const id = t.id;
		if (e.shiftKey && anchor !== null) {
			const a = tasks.findIndex((x) => x.id === anchor);
			const b = tasks.findIndex((x) => x.id === id);
			if (a >= 0 && b >= 0) {
				if (!e.ctrlKey) sel.clear();
				for (const x of tasks.slice(Math.min(a, b), Math.max(a, b) + 1)) sel.add(x.id);
				return;
			}
		}
		if (e.ctrlKey || e.metaKey) {
			if (sel.has(id)) sel.delete(id);
			else sel.add(id);
		} else {
			sel.clear();
			sel.add(id);
		}
		anchor = id;
	}

	function context(e: MouseEvent, t: Task) {
		e.preventDefault();
		if (!downloads.selected.has(t.id)) {
			downloads.selected.clear();
			downloads.selected.add(t.id);
			anchor = t.id;
		}
		oncontext(e, t);
	}

	function statusText(r: Row): string {
		const t = r.task;
		switch (t.status) {
			case 'downloading':
				if (r.live?.phase === 'verifying') {
					const v = r.size ? Math.floor((r.live.verified / r.size) * 100) : 0;
					return `Verifying ${v}%`;
				}
				return r.task.path ? 'Downloading' : 'Connecting';
			case 'queued':
				// Waiting for an automatic retry after a failure that may pass.
				return t.error ? 'Retrying' : 'Queued';
			case 'paused':
				return 'Paused';
			case 'done':
				return 'Complete';
			case 'error':
				return t.error_kind === 'auth'
					? 'Login needed'
					: t.error_kind === 'expired'
						? 'Link expired'
						: 'Error';
		}
	}
</script>

<div class="scroll">
	<table>
		<colgroup>
			<col class="c-queue" /><col class="c-name" /><col class="c-size" /><col class="c-progress" /><col
				class="c-speed"
			/><col class="c-eta" /><col class="c-status" /><col class="c-added" />
		</colgroup>
		<thead>
			<tr>
				{#each COLUMNS as c (c.key)}
					<th
						class:numeric={c.numeric}
						aria-sort={sortKey === c.key ? (sortDesc ? 'descending' : 'ascending') : 'none'}
					>
						<button onclick={() => sortBy(c.key)}>
							{c.label}
							{#if sortKey === c.key}<span class="arrow">{sortDesc ? '▾' : '▴'}</span>{/if}
						</button>
					</th>
				{/each}
			</tr>
		</thead>
		<tbody>
			{#each tasks as t (t.id)}
				{@const r = row(t, downloads.live[t.id])}
				<tr
					class:selected={downloads.selected.has(t.id)}
					class="s-{t.status}"
					onclick={(e) => select(e, t)}
					ondblclick={() => onopen(t)}
					oncontextmenu={(e) => context(e, t)}
				>
					<td class="numeric muted">{queuePos.get(t.id) ?? ''}</td>
					<td class="name" title={r.task.url}>{r.name}</td>
					<td class="numeric">{bytes(r.size)}</td>
					<td>
						<div class="bar" role="progressbar" aria-valuenow={Math.round((r.fraction ?? 0) * 100)}>
							<div class="fill" style:width="{(r.fraction ?? 0) * 100}%"></div>
							<span
								>{r.fraction !== null
									? `${(r.fraction * 100).toFixed(1)}%`
									: r.received > 0
										? bytes(r.received)
										: ''}</span
							>
						</div>
					</td>
					<td class="numeric">{speed(r.speed)}</td>
					<td class="numeric">{duration(r.eta)}</td>
					<td class="status" title={r.task.error ?? ''}>{statusText(r)}</td>
					<td class="numeric">{date(r.task.added_at)}</td>
				</tr>
			{/each}
		</tbody>
	</table>
	{#if downloads.loaded && tasks.length === 0}
		<p class="empty">Nothing here. Paste a link (Ctrl+V) or drop one on the window.</p>
	{/if}
</div>

<style>
	.scroll {
		overflow: auto;
		height: 100%;
	}
	table {
		width: 100%;
		border-collapse: collapse;
		table-layout: fixed;
		font-variant-numeric: tabular-nums;
	}
	.c-queue {
		width: 38px;
	}
	.c-name {
		width: auto;
	}
	.muted {
		color: var(--muted);
	}
	tr.selected .muted {
		color: inherit;
	}
	.c-size,
	.c-speed {
		width: 92px;
	}
	.c-progress {
		width: 150px;
	}
	.c-eta {
		width: 100px;
	}
	.c-status {
		width: 110px;
	}
	.c-added {
		width: 90px;
	}
	thead th {
		text-align: left;
		position: sticky;
		top: 0;
		z-index: 1;
		background: var(--panel);
		border-bottom: 1px solid var(--line);
		padding: 0;
		font-weight: 500;
	}
	th button {
		width: 100%;
		border: 0;
		background: transparent;
		color: var(--muted);
		font: inherit;
		text-align: inherit;
		padding: 6px 8px;
		cursor: pointer;
	}
	th.numeric button {
		text-align: right;
	}
	th button:hover {
		color: var(--fg);
	}
	.arrow {
		font-size: 10px;
	}
	td {
		padding: 5px 8px;
		border-bottom: 1px solid var(--line-soft);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	th.numeric,
	td.numeric {
		text-align: right;
	}
	tbody tr {
		cursor: default;
		user-select: none;
	}
	tbody tr:hover {
		background: var(--hover);
	}
	tbody tr.selected {
		background: var(--selected);
		color: var(--selected-fg);
	}
	tr.s-error .status {
		color: var(--danger);
	}
	tr.selected.s-error .status {
		color: inherit;
	}
	.bar {
		position: relative;
		height: 16px;
		border-radius: 3px;
		background: var(--track);
		overflow: hidden;
	}
	/* Soft fills so the percentage stays readable on top. */
	.fill {
		height: 100%;
		background: color-mix(in srgb, var(--accent) 45%, transparent);
		/* No width transition: animating 20 bars kept the UI at ~70% of a core. */
	}
	tr.s-done .fill {
		background: color-mix(in srgb, var(--ok) 45%, transparent);
	}
	tr.s-paused .fill,
	tr.s-queued .fill {
		background: color-mix(in srgb, var(--muted) 40%, transparent);
	}
	tr.s-error .fill {
		background: color-mix(in srgb, var(--danger) 45%, transparent);
	}
	.bar span {
		position: absolute;
		inset: 0;
		font-size: 11px;
		line-height: 16px;
		text-align: center;
		color: var(--fg);
	}
	.empty {
		color: var(--muted);
		text-align: center;
		margin-top: 48px;
	}
</style>
