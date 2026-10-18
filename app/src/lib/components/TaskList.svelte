<script lang="ts">
	import { Button, Icon, Progress } from './ui/index.ts';
	import type { Task, TaskId } from '../api.ts';
	import { downloads, row, type Row, type SortKey } from '../downloads.svelte.ts';
	import { bytes, duration, percent, speed } from '../format.ts';
	import { msg, t } from '../i18n.svelte.ts';

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
		{ key: 'name', label: msg('Name') },
		{ key: 'size', label: msg('Size'), numeric: true },
		{ key: 'progress', label: msg('Progress') },
		{ key: 'speed', label: msg('Speed'), numeric: true },
		{ key: 'eta', label: msg('Time left'), numeric: true },
		{ key: 'status', label: msg('Status') }
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

	/** The file's extension, as a short tag before its name. */
	function kind(name: string): string {
		const dot = name.lastIndexOf('.');
		const ext = dot > 0 ? name.slice(dot + 1) : '';
		return ext.length && ext.length <= 4 ? ext.toUpperCase() : '';
	}

	function statusText(r: Row): string {
		const task = r.task;
		switch (task.status) {
			case 'downloading':
				if (r.live?.phase === 'verifying') {
					const v = r.size ? Math.floor((r.live.verified / r.size) * 100) : 0;
					return t('Verifying {v}%', { v });
				}
				return r.task.path ? t('Downloading') : t('Connecting');
			case 'queued':
				// Waiting for an automatic retry after a failure that may pass.
				return task.error ? t('Retrying') : t('Waiting');
			case 'paused':
				return t('Paused');
			case 'done':
				return t('Done');
			case 'error':
				return task.error_kind === 'auth'
					? t('Login needed')
					: task.error_kind === 'expired'
						? t('Link expired')
						: t('Failed');
		}
	}
</script>

<div class="scroll">
	<table>
		<colgroup>
			<col class="c-queue" /><col class="c-name" /><col class="c-size" /><col class="c-progress" /><col
				class="c-speed"
			/><col class="c-eta" /><col class="c-status" />
		</colgroup>
		<thead>
			<tr>
				{#each COLUMNS as c (c.key)}
					<th
						class:numeric={c.numeric}
						aria-sort={sortKey === c.key ? (sortDesc ? 'descending' : 'ascending') : 'none'}
					>
						<Button variant="column" align={c.numeric ? 'end' : 'start'} onclick={() => sortBy(c.key)}>
							{t(c.label)}
							{#if sortKey === c.key}<Icon name={sortDesc ? 'arrow-down' : 'arrow-up'} />{/if}
						</Button>
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
					<td class="name" title={r.task.url}>
						<span class="kind">{kind(r.name)}</span>
						{r.name}
					</td>
					<td class="numeric">{bytes(r.size)}</td>
					<td>
						<Progress
							value={r.fraction}
							state={t.status === 'done'
								? 'done'
								: t.status === 'error'
									? 'error'
									: t.status === 'paused' || t.status === 'queued'
										? 'stopped'
										: 'running'}
							text={r.fraction !== null ? percent(r.fraction) : r.received > 0 ? bytes(r.received) : ''}
						/>
					</td>
					<td class="numeric">{speed(r.speed)}</td>
					<td class="numeric">{duration(r.eta)}</td>
					<td class="status" title={r.task.error ?? ''}>
						<span class="marker"></span>{statusText(r)}
					</td>
				</tr>
			{/each}
		</tbody>
	</table>
	{#if downloads.loaded && tasks.length === 0}
		<div class="empty">
			<h2>{t('No downloads here')}</h2>
			<p>{t('Paste a link with Ctrl+V, drop one on the window, or choose New download.')}</p>
		</div>
	{/if}
</div>

<style>
	.scroll {
		height: 100%;
		overflow: auto;
	}
	table {
		width: 100%;
		border-collapse: collapse;
		table-layout: fixed;
	}
	.c-queue {
		width: 40px;
	}
	.c-name {
		width: auto;
	}
	.c-size,
	.c-speed {
		width: 88px;
	}
	.c-progress {
		width: 160px;
	}
	.c-eta {
		width: 88px;
	}
	.c-status {
		width: 144px;
	}
	thead th {
		position: sticky;
		top: 0;
		z-index: 1;
		height: var(--space-8);
		padding: 0;
		border-bottom: 1px solid var(--line);
		background: var(--bg);
		text-align: left;
	}
	td {
		padding: 0 var(--space-2);
		border-bottom: 1px solid var(--line-soft);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	td.numeric {
		font-family: var(--font-mono);
		font-size: var(--text-caption);
		font-variant-numeric: tabular-nums;
		text-align: right;
	}
	.muted {
		color: var(--faint);
	}
	tbody tr {
		height: var(--space-10);
		cursor: default;
		user-select: none;
	}
	tbody tr:hover {
		background: var(--hover);
	}
	tbody tr.selected {
		background: var(--selected);
		box-shadow: inset 2px 0 0 var(--accent);
	}
	.name {
		font-weight: 500;
	}
	.kind {
		display: inline-block;
		min-width: var(--space-8);
		margin-right: var(--space-2);
		padding: 0 var(--space-1);
		border: 1px solid var(--line);
		border-radius: var(--radius-1);
		color: var(--muted);
		font-family: var(--font-mono);
		font-size: var(--text-caption);
		font-weight: 500;
		text-align: center;
		vertical-align: 1px;
	}
	.kind:empty {
		visibility: hidden;
	}
	.status {
		color: var(--muted);
		font-family: var(--font-mono);
		font-size: var(--text-caption);
		letter-spacing: 0.04em;
		text-transform: uppercase;
	}
	.marker {
		display: inline-block;
		width: var(--space-2);
		height: var(--space-2);
		margin-right: var(--space-2);
		border: 1px solid var(--faint);
		vertical-align: 0;
	}
	.s-downloading .status {
		color: var(--fg);
	}
	.s-downloading .marker {
		border-color: var(--accent);
		background: var(--accent);
	}
	.s-done .marker {
		border-color: var(--faint);
		background: var(--faint);
	}
	.s-error .status {
		color: var(--accent);
	}
	.s-error .marker {
		border-color: var(--accent);
	}
	.empty {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: var(--space-2);
		margin-top: var(--space-14);
		text-align: center;
	}
	.empty h2 {
		font-size: var(--text-h2);
		font-weight: 600;
	}
	.empty p {
		max-width: 360px;
		color: var(--muted);
	}
</style>
