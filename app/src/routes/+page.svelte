<script lang="ts">
	import { onMount } from 'svelte';
	import {
		getSettings,
		moveTask,
		onCapture,
		onUpdate,
		onClipboard,
		showProgress,
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
	import GrabDialog from '#lib/components/GrabDialog.svelte';
	import MediaDialog from '#lib/components/MediaDialog.svelte';
	import PageDialog from '#lib/components/PageDialog.svelte';
	import SettingsDialog from '#lib/components/SettingsDialog.svelte';
	import Sidebar from '#lib/components/Sidebar.svelte';
	import TaskList from '#lib/components/TaskList.svelte';
	import { Button, Input, Select } from '#lib/components/ui/index.ts';
	import {
		compare,
		downloads,
		inView,
		row,
		type SortKey,
		type View
	} from '#lib/downloads.svelte.ts';
	import { speed } from '#lib/format.ts';
	import { msg, t } from '#lib/i18n.svelte.ts';

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
	let grabbing = $state(false);
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
					label: msg('Open'),
					action: () => run(openFile(task.id)),
					disabled: !one || one.status !== 'done'
				},
				{
					label: msg('Show in folder'),
					action: () => run(showInFolder(task.id)),
					disabled: !one || !one.path
				},
				{
					label: msg('Progress window'),
					action: () => run(showProgress(task.id)),
					disabled: !one
				},
				{
					label: msg('Resume'),
					action: () => run(startTasks(ids(canStart))),
					disabled: !sel.some(canStart),
					separated: true
				},
				{
					label: msg('Pause'),
					action: () => run(pauseTasks(ids(canPause))),
					disabled: !sel.some(canPause)
				},
				{
					label: msg('Move to top'),
					action: () => run(moveTask(task.id, 'top')),
					disabled: !one || one.status === 'done',
					separated: true
				},
				{
					label: msg('Move up'),
					action: () => run(moveTask(task.id, 'up')),
					disabled: !one || one.status === 'done'
				},
				{
					label: msg('Move down'),
					action: () => run(moveTask(task.id, 'down')),
					disabled: !one || one.status === 'done'
				},
				{
					label: msg('Move to bottom'),
					action: () => run(moveTask(task.id, 'bottom')),
					disabled: !one || one.status === 'done'
				},
				{
					label: msg('Copy address'),
					action: () => run(navigator.clipboard.writeText(sel.map((t) => t.url).join('\n'))),
					separated: true
				},
				{
					label: msg('Download again'),
					action: () => run(restartTask(task.id)),
					disabled: !one
				},
				{
					label: msg('Remove…'),
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
		<Button variant="primary" onclick={() => (adding = { url: '' })} title="Ctrl+N">{t('Add URL')}</Button>
		<Button onclick={() => (grabbing = true)} title={t('Find the files a site links to')}
			>{t('Site grabber')}</Button
		>
		<Button onclick={() => run(startTasks(ids(canStart)))} disabled={!selectedTasks.some(canStart)}
			>{t('Resume')}</Button
		>
		<Button onclick={() => run(pauseTasks(ids(canPause)))} disabled={!selectedTasks.some(canPause)}
			>{t('Pause')}</Button
		>
		<Button
			onclick={() => (removing = selectedTasks.map((t) => t.id))}
			disabled={!selectedTasks.length}
			title="Delete">{t('Remove')}</Button
		>
		<span class="sep"></span>
		<Button onclick={() => run(startAll())} title={t('Queue every paused or failed download')}
			>{t('Start all')}</Button
		>
		<Button onclick={() => run(pauseAll())} title={t('Pause every running and queued download')}
			>{t('Pause all')}</Button
		>
		<label class="when" title={t('One time: when nothing is downloading or waiting any more')}>
			{t('When done')}
			<Select
				variant="inline"
				value={whenDone}
				onchange={(e) => run(setWhenDone(e.currentTarget.value as WhenDone))}
			>
				<option value="nothing">{t('do nothing')}</option>
				<option value="exit">{t('close the app')}</option>
				<option value="sleep">{t('sleep')}</option>
				<option value="hibernate">{t('hibernate')}</option>
				<option value="shutdown">{t('shut down')}</option>
			</Select>
		</label>
		<span class="sep"></span>
		<Button
			pressed={showDetails}
			onclick={() => (showDetails = !showDetails)}
			title={t('Details of the selected download')}>{t('Details')}</Button
		>
		<span class="push"></span>
		<Button icon="settings" onclick={() => (settingsOpen = true)} title={t('Settings')} label={t('Settings')} />
		<Input variant="search" type="search" bind:value={search} placeholder={t('Search')} label={t('Search')} />
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
			<span class="info"
				>{t('idm-alternative {version} is available.', { version: update.version })}</span
			>
			<Button onclick={applyUpdate} disabled={updating}
				>{updating ? t('Downloading…') : t('Install and restart')}</Button
			>
			<span>{t('Running downloads pause and continue after the restart.')}</span>
			<Button variant="ghost" icon="close" onclick={() => (update = null)} label={t('Later')} />
		{:else if info}
			<span class="info">{info}</span>
			<Button variant="ghost" icon="close" onclick={() => (info = null)} label={t('Dismiss')} />
		{:else}
			<span>{t('{n} downloading', { n: downloads.active })}</span>
			{#if downloads.speed > 0}<span>{speed(downloads.speed)}</span>{/if}
			{#if globalLimit}<span title={t('Speed limit for all downloads')}
					>{t('limit {speed}', { speed: speed(globalLimit) })}</span
				>{/if}
			<span class="right">
				{visible.length === 1 ? t('1 item') : t('{n} items', { n: visible.length })}{downloads
					.selected.size
					? ' · ' + t('{n} selected', { n: downloads.selected.size })
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

{#if grabbing}
	<GrabDialog
		onfound={(found) => {
			grabbing = false;
			picking = found;
		}}
		onclose={() => (grabbing = false)}
	/>
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
		grid-template-columns: 192px 1fr;
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
		gap: var(--space-2);
		padding: var(--space-2) var(--space-3);
		border-bottom: 1px solid var(--line);
		background: var(--panel);
		white-space: nowrap;
	}
	.push {
		margin-left: auto;
	}
	header :global(input) {
		max-width: 224px;
	}
	:global(nav) {
		grid-area: side;
	}
	main {
		grid-area: main;
		display: grid;
		grid-template-rows: 1fr;
		min-height: 0;
		min-width: 0;
	}
	main.split {
		grid-template-rows: 1fr 232px;
	}
	.when {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		color: var(--muted);
	}
	.sep {
		align-self: stretch;
		width: 1px;
		margin: var(--space-1) 0;
		background: var(--line);
	}
	footer {
		grid-area: foot;
		display: flex;
		align-items: center;
		gap: var(--space-4);
		min-height: var(--space-8);
		padding: 0 var(--space-3);
		border-top: 1px solid var(--line);
		background: var(--panel);
		color: var(--muted);
		font-size: var(--text-caption);
		font-variant-numeric: tabular-nums;
	}
	.right {
		margin-left: auto;
	}
	.notice {
		color: var(--accent);
	}
	.info {
		color: var(--fg);
	}
</style>
