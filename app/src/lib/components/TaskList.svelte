<script lang="ts">
	import { Button, Icon, Progress } from './ui/index.ts';
	import type { Group, Task, TaskId } from '../api.ts';
	import { downloads, row, type Row, type SortKey } from '../downloads.svelte.ts';
	import { bytes, duration, percent, speed } from '../format.ts';
	import { msg, t } from '../i18n.svelte.ts';

	// Gets the tasks already filtered and in order; each row reads its own live progress, so a
	// progress update re-renders only the rows of running downloads.
	let {
		tasks,
		sortKey = $bindable(),
		sortDesc = $bindable(),
		reveal = $bindable(null),
		oncontext,
		onopen
	}: {
		tasks: Task[];
		sortKey: SortKey;
		sortDesc: boolean;
		/** A download to scroll into view; cleared once it is. */
		reveal?: TaskId | null;
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

	/** A download, or the row of downloads added together (a playlist, a batch), which folds
	 * them into one until opened. */
	type ListRow =
		| { kind: 'task'; task: Task; member: boolean }
		| { kind: 'group'; group: Group; tasks: Task[]; open: boolean };

	// A group's downloads stay together where its first one is; a group of one is a plain row.
	const rows = $derived.by(() => {
		const members = new Map<number, Task[]>();
		for (const t of tasks) {
			if (t.group_id === null || !downloads.groups[t.group_id]) continue;
			const list = members.get(t.group_id) ?? [];
			list.push(t);
			members.set(t.group_id, list);
		}
		const out: ListRow[] = [];
		const placed = new Set<number>();
		for (const t of tasks) {
			const list = t.group_id === null ? undefined : members.get(t.group_id);
			if (!list || list.length < 2) {
				out.push({ kind: 'task', task: t, member: false });
				continue;
			}
			const id = t.group_id!;
			if (placed.has(id)) continue;
			placed.add(id);
			const open = downloads.opened.has(id);
			out.push({ kind: 'group', group: downloads.groups[id], tasks: list, open });
			if (open) for (const m of list) out.push({ kind: 'task', task: m, member: true });
		}
		return out;
	});

	/** A group's downloads summed up like one download. */
	function total(group: Task[]) {
		const rs = group.map((t) => row(t, downloads.live[t.id]));
		const known = rs.every((r) => r.size !== null);
		const size = known ? rs.reduce((s, r) => s + (r.size ?? 0), 0) : null;
		const received = rs.reduce((s, r) => s + r.received, 0);
		const speed = rs.reduce((s, r) => s + r.speed, 0);
		const done = group.filter((t) => t.status === 'done').length;
		const failed = group.filter((t) => t.status === 'error').length;
		return {
			size,
			received,
			speed,
			fraction: size ? Math.min(1, received / size) : done / group.length,
			eta: size && speed > 0 ? (size - received) / speed : null,
			done,
			failed,
			running: group.some((t) => t.status === 'downloading')
		};
	}

	function selectGroup(e: MouseEvent, group: Task[]) {
		if (!e.ctrlKey && !e.metaKey) downloads.selected.clear();
		for (const t of group) downloads.selected.add(t.id);
		anchor = group[0].id;
	}

	function toggle(id: number) {
		if (downloads.opened.has(id)) downloads.opened.delete(id);
		else downloads.opened.add(id);
	}

	// Only the rows in view (and a few around them) are in the page; empty rows above and below
	// keep the scroll height. A long list otherwise costs the WebView ~0.1 MB per row.
	const OVERSCAN = 8;
	let scroller: HTMLDivElement;
	let scrollTop = $state(0);
	let viewHeight = $state(0);
	/** Measured from a drawn row; the CSS height plus its share of the border. */
	let rowHeight = $state(40);
	const first = $derived(Math.max(0, Math.floor(scrollTop / rowHeight) - OVERSCAN));
	const last = $derived(
		Math.min(rows.length, Math.ceil((scrollTop + viewHeight) / rowHeight) + OVERSCAN)
	);
	const shown = $derived(rows.slice(first, last));

	$effect(() => {
		void shown;
		const h = scroller.querySelector('tbody tr:not(.pad)')?.getBoundingClientRect().height;
		if (h && Math.abs(h - rowHeight) > 0.01) rowHeight = h;
	});

	$effect(() => {
		if (reveal === null) return;
		const task = tasks.find((t) => t.id === reveal);
		// In a folded group: opened first, and this runs again.
		if (task?.group_id != null && rows.some((r) => r.kind === 'group' && r.group.id === task.group_id && !r.open)) {
			downloads.opened.add(task.group_id);
			return;
		}
		const i = rows.findIndex((r) => r.kind === 'task' && r.task.id === reveal);
		if (i < 0) return;
		const top = i * rowHeight;
		if (top < scroller.scrollTop || top + rowHeight > scroller.scrollTop + viewHeight) {
			scroller.scrollTop = Math.max(0, top - viewHeight / 2);
		}
		reveal = null;
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
		const moved = downloads.moving[task.id];
		if (moved !== undefined) return t('Moving {p}%', { p: Math.floor(moved * 100) });
		switch (task.status) {
			case 'downloading':
				if (r.live?.phase === 'settingup') {
					const { received, size } = r.live;
					const v = size ? Math.floor((received / size) * 100) : 0;
					return t('Setting up ffmpeg {v}%', { v });
				}
				if (r.live?.phase === 'verifying') {
					const v = r.size ? Math.floor((r.live.verified / r.size) * 100) : 0;
					const from = r.live.checksum_from;
					return from
						? t('Verifying (checksum from {from}) {v}%', { from, v })
						: t('Verifying {v}%', { v });
				}
				return r.task.path ? t('Downloading') : t('Connecting');
			case 'queued':
				if (task.error_kind === 'offline') return t('Waiting for the network');
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

<div
	class="scroll"
	bind:this={scroller}
	bind:clientHeight={viewHeight}
	onscroll={() => (scrollTop = scroller.scrollTop)}
>
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
			{#if first > 0}
				<tr class="pad" style:height="{first * rowHeight}px"><td colspan="7"></td></tr>
			{/if}
			{#each shown as item (item.kind === 'task' ? `t${item.task.id}` : `g${item.group.id}`)}
				{#if item.kind === 'group'}
					{@const g = total(item.tasks)}
					<tr
						class="group"
						class:s-downloading={g.running}
						class:s-done={g.done === item.tasks.length}
						class:s-error={g.failed > 0 && !g.running}
						class:selected={item.tasks.every((t) => downloads.selected.has(t.id))}
						onclick={(e) => selectGroup(e, item.tasks)}
						ondblclick={() => toggle(item.group.id)}
						oncontextmenu={(e) => {
							if (!item.tasks.every((t) => downloads.selected.has(t.id))) selectGroup(e, item.tasks);
							e.preventDefault();
							oncontext(e, item.tasks[0]);
						}}
					>
						<td class="numeric">
							<Button
								variant="ghost"
								icon={item.open ? 'chevron-down' : 'chevron-right'}
								label={item.open ? t('Fold the group') : t('Show the group')}
								onclick={(e) => {
									e.stopPropagation();
									toggle(item.group.id);
								}}
							/>
						</td>
						<td class="name" title={item.group.name}>
							<span class="kind">{item.tasks.length}</span>
							{item.group.name}
						</td>
						<td class="numeric">{bytes(g.size)}</td>
						<td>
							<Progress
								value={g.fraction}
								state={g.done === item.tasks.length
									? 'done'
									: g.failed > 0 && !g.running
										? 'error'
										: g.running
											? 'running'
											: 'stopped'}
								text={percent(g.fraction)}
							/>
						</td>
						<td class="numeric">{speed(g.speed)}</td>
						<td class="numeric">{duration(g.eta)}</td>
						<td class="status">
							<span class="marker"></span>{t('{done} of {n} done', { done: g.done, n: item.tasks.length })}
						</td>
					</tr>
				{:else}
				{@const t = item.task}
				{@const r = row(t, downloads.live[t.id])}
				<tr
					class:selected={downloads.selected.has(t.id)}
					class:member={item.member}
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
				{/if}
			{/each}
			{#if last < rows.length}
				<tr class="pad" style:height="{(rows.length - last) * rowHeight}px"><td colspan="7"></td></tr>
			{/if}
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
	tbody tr.pad:hover {
		background: none;
	}
	tr.pad td {
		padding: 0;
		border: 0;
	}
	tbody tr.selected {
		background: var(--selected);
		box-shadow: inset 2px 0 0 var(--accent);
	}
	.name {
		font-weight: 500;
	}
	/* A download inside an opened group sits under the group's row. */
	.member .name {
		padding-left: var(--space-6);
	}
	.group .name {
		font-weight: 600;
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
		border-color: var(--success);
		background: var(--success);
	}
	.s-error .status {
		color: var(--danger);
	}
	.s-error .marker {
		border-color: var(--danger);
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
