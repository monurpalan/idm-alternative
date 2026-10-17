<script lang="ts">
	import { onMount } from 'svelte';
	import {
		getSettings,
		moveTask,
		onCapture,
		onUpdate,
		onClipboard,
		pageReady,
		getWhenDone,
		setWhenDone,
		onWhenDone,
		onCountdown,
		type WhenDone,
		installUpdate,
		type AppUpdate,
		onCapturedLinks,
		onCapturedMedia,
		onNotice,
		onSettings,
		openFile,
		pauseAll,
		pauseTasks,
		removeTasks,
		restartTask,
		showInFolder,
		startAll,
		startTasks,
		type CapturedLinks,
		type CapturedMedia,
		type Headers,
		type Task,
		type TaskId
	} from '#lib/api.ts';
	import AddDialog from '#lib/components/AddDialog.svelte';
	import ConfirmRemove from '#lib/components/ConfirmRemove.svelte';
	import Countdown from '#lib/components/Countdown.svelte';
	import ContextMenu, { type Item } from '#lib/components/ContextMenu.svelte';
	import DetailPane from '#lib/components/DetailPane.svelte';
	import LinksDialog from '#lib/components/LinksDialog.svelte';
	import MediaDialog from '#lib/components/MediaDialog.svelte';
	import PageDialog from '#lib/components/PageDialog.svelte';
	import SettingsDialog from '#lib/components/SettingsDialog.svelte';
	import Sidebar from '#lib/components/Sidebar.svelte';
	import TaskList from '#lib/components/TaskList.svelte';
	import {
		compare,
		downloads,
		inView,
		row,
		type SortKey,
		type View
	} from '#lib/downloads.svelte.ts';
	import { speed } from '#lib/format.ts';

	let view = $state<View>('all');
	let search = $state('');
	let sortKey = $state<SortKey>('queue');
	let sortDesc = $state(false);
	let adding = $state<{
		url: string;
		name?: string | null;
		headers?: Headers | null;
	} | null>(null);
	let picking = $state<CapturedLinks | null>(null);
	let streaming = $state<CapturedMedia | null>(null);
	let pageMedia = $state<CapturedMedia | null>(null);
	let removing = $state<TaskId[] | null>(null);
	let menu = $state<{ x: number; y: number; items: Item[] } | null>(null);
	let notice = $state<string | null>(null);
	/** A passing message that is not an error. */
	let info = $state<string | null>(null);
	let showDetails = $state(true);
	let settingsOpen = $state(false);
	let globalLimit = $state<number | null>(null);
	/** A newer version of the app, found by the daily check. */
	let update = $state<AppUpdate | null>(null);
	let updating = $state(false);
	/** What happens when all downloads finish (one shot). */
	let whenDone = $state<WhenDone>('nothing');
	let countdown = $state<{ action: WhenDone; seconds: number } | null>(null);

	async function applyUpdate() {
		updating = true;
		try {
			// The app closes here and the new version starts.
			await installUpdate();
		} catch (e) {
			notice = String(e);
			updating = false;
		}
	}

	onMount(() => {
		const stop = downloads.connect();
		getSettings()
			.then((s) => (globalLimit = s.speed_limit))
			.catch(() => {});
		getWhenDone()
			.then((a) => (whenDone = a))
			.catch(() => {});
		const unlisten = [
			onSettings((s) => (globalLimit = s.speed_limit)),
			// The browser handed over a download: confirm it like IDM does.
			onCapture((c) => (adding = { url: c.url, name: c.filename, headers: c.headers })),
			onCapturedLinks((l) => (picking = l)),
			onNotice((text) => (info = text)),
			onUpdate((u) => (update = u)),
			// A copied link: offer it, unless the user is already adding something.
			onClipboard((url) => {
				if (!adding) adding = { url, name: null, headers: null };
			}),
			onWhenDone((a) => (whenDone = a)),
			onCountdown((c) => (countdown = c)),
			onCapturedMedia((m) => {
				const item = m.items[0];
				if (!item) return;
				if (item.kind === 'video' || item.kind === 'audio') {
					adding = { url: item.url, name: null, headers: m.headers };
				} else if (item.kind === 'page') {
					pageMedia = m;
				} else {
					streaming = m;
				}
			})
		];
		// Only once every listener is in place: a capture that opened the window must not be
		// missed while it loads.
		Promise.all(unlisten)
			.then(() => pageReady())
			.catch(() => {});
		return () => {
			stop();
			unlisten.forEach((u) => u.then((f) => f()));
		};
	});

	/** Sort keys that change with live progress; only these make the order depend on it. */
	const LIVE_KEYS: SortKey[] = ['progress', 'speed', 'eta'];

	const visible = $derived.by(() => {
		const q = search.trim().toLowerCase();
		const live = LIVE_KEYS.includes(sortKey);
		const list = downloads.tasks
			.filter(
				(t) =>
					inView(t, view) &&
					(!q || (t.name ?? '').toLowerCase().includes(q) || t.url.toLowerCase().includes(q))
			)
			.map((t) => row(t, live ? downloads.live[t.id] : undefined));
		list.sort((a, b) => compare(a, b, sortKey) * (sortDesc ? -1 : 1));
		return list.map((r) => r.task);
	});

	const selectedTasks = $derived(
		downloads.tasks.filter((t) => downloads.selected.has(t.id))
	);
	const detailed = $derived(showDetails && selectedTasks.length === 1 ? selectedTasks[0] : null);
	const canStart = (t: Task) => t.status === 'paused' || t.status === 'error';
	const canPause = (t: Task) => t.status === 'downloading' || t.status === 'queued';

	function ids(pred: (t: Task) => boolean): TaskId[] {
		return selectedTasks.filter(pred).map((t) => t.id);
	}

	/** Runs an action, showing its error in the status bar instead of losing it. */
	async function run(p: Promise<unknown>) {
		try {
			await p;
			notice = null;
		} catch (e) {
			notice = String(e);
		}
	}

	function openRow(t: Task) {
		if (t.status === 'done') run(openFile(t.id));
	}

	function contextMenu(e: MouseEvent, task: Task) {
		const sel = selectedTasks;
		const one = sel.length === 1 ? sel[0] : null;
		menu = {
			x: e.clientX,
			y: e.clientY,
			items: [
				{
					label: 'Open',
					action: () => run(openFile(task.id)),
					disabled: !one || one.status !== 'done'
				},
				{
					label: 'Show in folder',
					action: () => run(showInFolder(task.id)),
					disabled: !one || !one.path
				},
				{
					label: 'Resume',
					action: () => run(startTasks(ids(canStart))),
					disabled: !sel.some(canStart),
					separated: true
				},
				{
					label: 'Pause',
					action: () => run(pauseTasks(ids(canPause))),
					disabled: !sel.some(canPause)
				},
				{
					label: 'Move to top',
					action: () => run(moveTask(task.id, 'top')),
					disabled: !one || one.status === 'done',
					separated: true
				},
				{
					label: 'Move up',
					action: () => run(moveTask(task.id, 'up')),
					disabled: !one || one.status === 'done'
				},
				{
					label: 'Move down',
					action: () => run(moveTask(task.id, 'down')),
					disabled: !one || one.status === 'done'
				},
				{
					label: 'Move to bottom',
					action: () => run(moveTask(task.id, 'bottom')),
					disabled: !one || one.status === 'done'
				},
				{
					label: 'Copy address',
					action: () => run(navigator.clipboard.writeText(sel.map((t) => t.url).join('\n'))),
					separated: true
				},
				{
					label: 'Download again',
					action: () => run(restartTask(task.id)),
					disabled: !one
				},
				{
					label: 'Remove…',
					action: () => (removing = sel.map((t) => t.id)),
					danger: true,
					separated: true
				}
			]
		};
	}

	function looksLikeUrl(s: string): boolean {
		return /^(https?|ftp):\/\/\S+$/i.test(s.trim());
	}

	function inField(e: Event): boolean {
		const t = e.target as HTMLElement | null;
		return !!t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable);
	}

	function keydown(e: KeyboardEvent) {
		if (adding || removing || settingsOpen || picking || streaming || pageMedia || inField(e)) return;
		const ctrl = e.ctrlKey || e.metaKey;
		if (ctrl && e.key.toLowerCase() === 'a') {
			e.preventDefault();
			for (const t of visible) downloads.selected.add(t.id);
		} else if (ctrl && e.key.toLowerCase() === 'n') {
			e.preventDefault();
			adding = { url: '' };
		} else if (e.key === 'Delete' && selectedTasks.length) {
			removing = selectedTasks.map((t) => t.id);
		} else if (e.key === 'Enter' && selectedTasks.length === 1) {
			const t = selectedTasks[0];
			if (t.status === 'done') run(openFile(t.id));
		} else if (e.key === 'Escape') {
			downloads.selected.clear();
		}
	}

	function paste(e: ClipboardEvent) {
		if (adding || removing || settingsOpen || picking || inField(e)) return;
		const text = e.clipboardData?.getData('text/plain') ?? '';
		if (looksLikeUrl(text)) {
			e.preventDefault();
			adding = { url: text.trim() };
		}
	}

	function drop(e: DragEvent) {
		e.preventDefault();
		const text =
			e.dataTransfer?.getData('text/uri-list').split('\n').find((l) => l && !l.startsWith('#')) ||
			e.dataTransfer?.getData('text/plain') ||
			'';
		if (looksLikeUrl(text)) adding = { url: text.trim() };
	}
</script>

<svelte:window onkeydown={keydown} onpaste={paste} ondragover={(e) => e.preventDefault()} ondrop={drop} />

<div class="app">
	<header>
		<button class="primary" onclick={() => (adding = { url: '' })} title="Ctrl+N">Add URL</button>
		<button
			onclick={() => run(startTasks(ids(canStart)))}
			disabled={!selectedTasks.some(canStart)}>Resume</button
		>
		<button
			onclick={() => run(pauseTasks(ids(canPause)))}
			disabled={!selectedTasks.some(canPause)}>Pause</button
		>
		<button
			onclick={() => (removing = selectedTasks.map((t) => t.id))}
			disabled={!selectedTasks.length}
			title="Delete">Remove</button
		>
		<span class="sep"></span>
		<button onclick={() => run(startAll())} title="Queue every paused or failed download"
			>Start all</button
		>
		<button onclick={() => run(pauseAll())} title="Pause every running and queued download"
			>Pause all</button
		>
		<label class="when" title="One time: when nothing is downloading or waiting any more">
			When done
			<select
				value={whenDone}
				onchange={(e) => run(setWhenDone(e.currentTarget.value as WhenDone))}
			>
				<option value="nothing">do nothing</option>
				<option value="exit">close the app</option>
				<option value="sleep">sleep</option>
				<option value="hibernate">hibernate</option>
				<option value="shutdown">shut down</option>
			</select>
		</label>
		<span class="sep"></span>
		<button
			class="toggle"
			aria-pressed={showDetails}
			onclick={() => (showDetails = !showDetails)}
			title="Details of the selected download">Details</button
		>
		<button class="icon" onclick={() => (settingsOpen = true)} title="Settings" aria-label="Settings"
			>⚙</button
		>
		<input class="search" type="search" bind:value={search} placeholder="Search" aria-label="Search" />
	</header>

	<Sidebar bind:view />

	<main class:split={detailed}>
		<TaskList tasks={visible} bind:sortKey bind:sortDesc oncontext={contextMenu} onopen={openRow} />
		{#if detailed}
			<DetailPane task={detailed} onerror={(m) => (notice = m)} />
		{/if}
	</main>

	<footer>
		{#if notice || downloads.error}
			<span class="notice">{notice ?? downloads.error}</span>
		{:else if update}
			<span class="info">idm-alternative {update.version} is available.</span>
			<button onclick={applyUpdate} disabled={updating}
				>{updating ? 'Downloading…' : 'Install and restart'}</button
			>
			<span>Running downloads pause and continue after the restart.</span>
			<button class="dismiss" onclick={() => (update = null)} aria-label="Later">×</button>
		{:else if info}
			<span class="info">{info}</span>
			<button class="dismiss" onclick={() => (info = null)} aria-label="Dismiss">×</button>
		{:else}
			<span>{downloads.active} downloading</span>
			{#if downloads.speed > 0}<span>{speed(downloads.speed)}</span>{/if}
			{#if globalLimit}<span title="Speed limit for all downloads">limit {speed(globalLimit)}</span>{/if}
			<span class="right">
				{visible.length}
				{visible.length === 1 ? 'item' : 'items'}{downloads.selected.size
					? ` · ${downloads.selected.size} selected`
					: ''}
			</span>
		{/if}
	</footer>
</div>

{#if countdown}
	<Countdown
		action={countdown.action}
		seconds={countdown.seconds}
		onclose={() => (countdown = null)}
	/>
{/if}

{#if adding}
	<AddDialog
		url={adding.url}
		name={adding.name}
		headers={adding.headers}
		onclose={() => (adding = null)}
	/>
{/if}

{#if removing}
	<ConfirmRemove
		count={removing.length}
		onconfirm={(deleteFiles) => {
			const list = removing ?? [];
			run(removeTasks(list, deleteFiles));
		}}
		onclose={() => (removing = null)}
	/>
{/if}

{#if pageMedia}
	<PageDialog media={pageMedia} onclose={() => (pageMedia = null)} />
{/if}

{#if streaming}
	<MediaDialog media={streaming} onclose={() => (streaming = null)} />
{/if}

{#if picking}
	<LinksDialog
		referrer={picking.referrer}
		links={picking.links}
		onclose={() => (picking = null)}
		onerror={(m) => (notice = m)}
	/>
{/if}

{#if settingsOpen}
	<SettingsDialog onclose={() => (settingsOpen = false)} />
{/if}

{#if menu}
	<ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
{/if}

<style>
	.app {
		display: grid;
		grid-template-columns: 190px 1fr;
		grid-template-rows: auto 1fr auto;
		grid-template-areas:
			'head head'
			'side main'
			'foot foot';
		height: 100vh;
	}
	header {
		grid-area: head;
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 8px 10px;
		border-bottom: 1px solid var(--line);
		background: var(--panel);
	}
	.icon {
		margin-left: auto;
		padding: 4px 9px;
		font-size: 15px;
		line-height: 1;
	}
	.search {
		width: 220px;
		padding: 5px 8px;
		border: 1px solid var(--line);
		border-radius: 5px;
		background: var(--field);
		color: var(--fg);
		font: inherit;
	}
	:global(nav) {
		grid-area: side;
	}
	main {
		grid-area: main;
		min-height: 0;
		min-width: 0;
		display: grid;
		grid-template-rows: 1fr;
	}
	main.split {
		grid-template-rows: 1fr 230px;
	}
	.when {
		display: flex;
		align-items: center;
		gap: 6px;
		color: var(--muted);
		white-space: nowrap;
	}
	.sep {
		width: 1px;
		align-self: stretch;
		margin: 2px 4px;
		background: var(--line);
	}
	.toggle[aria-pressed='true'] {
		background: var(--selected);
		color: var(--selected-fg);
		border-color: transparent;
	}
	footer {
		grid-area: foot;
		display: flex;
		gap: 16px;
		padding: 4px 10px;
		border-top: 1px solid var(--line);
		background: var(--panel);
		color: var(--muted);
		font-variant-numeric: tabular-nums;
	}
	.right {
		margin-left: auto;
	}
	.notice {
		color: var(--danger);
	}
	.info {
		color: var(--fg);
	}
	.dismiss {
		padding: 0 6px;
		border: 0;
		background: transparent;
		color: var(--muted);
	}
</style>
