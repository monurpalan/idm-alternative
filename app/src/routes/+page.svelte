<script lang="ts">
	import { onMount } from 'svelte';
	import {
		BROWSER_NAMES,
		dismissExtensionWarning,
		extensionStatus,
		getSettings,
		installExtension,
		onExtensions,
		type ExtensionStatus,
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
		isVideoPage,
		droppedFileOrigin,
		onOriginPick,
		type Origin,
		type AppUpdate,
		onCapturedLinks,
		onCapturedMedia,
		onNotice,
		onSettings,
		openFile,
		pauseAll,
		pauseTasks,
		removeTasks,
		diskNeeds,
		relocateTask,
		startAll,
		startTasks,
		type DriveNeed,
		type CapturedLinks,
		type CapturedMedia,
		type Headers,
		type Task,
		type TaskId
	} from '#lib/api.ts';
	import AddDialog from '#lib/components/AddDialog.svelte';
	import ConfirmRemove from '#lib/components/ConfirmRemove.svelte';
	import { bytes } from '#lib/format.ts';
	import DiskDialog from '#lib/components/DiskDialog.svelte';
	import Countdown from '#lib/components/Countdown.svelte';
	import ContextMenu, { type Item } from '#lib/components/ContextMenu.svelte';
	import DetailPane from '#lib/components/DetailPane.svelte';
	import RenameDialog from '#lib/components/RenameDialog.svelte';
	import LinksDialog from '#lib/components/LinksDialog.svelte';
	import GrabDialog from '#lib/components/GrabDialog.svelte';
	import MediaDialog from '#lib/components/MediaDialog.svelte';
	import PageDialog from '#lib/components/PageDialog.svelte';
	import OriginDialog from '#lib/components/OriginDialog.svelte';
	import SettingsDialog from '#lib/components/SettingsDialog.svelte';
	import ListHeader from '#lib/components/ListHeader.svelte';
	import Sidebar from '#lib/components/Sidebar.svelte';
	import TaskList from '#lib/components/TaskList.svelte';
	import { Button, Text } from '#lib/components/ui/index.ts';
	import {
		compare,
		downloads,
		inView,
		row,
		VIEWS,
		type SortKey,
		type View
	} from '#lib/downloads.svelte.ts';
	import { t } from '#lib/i18n.svelte.ts';
	import { canPause, canStart, taskMenu } from '#lib/taskMenu.ts';
	import { inField, looksLikeUrl, sha256 } from '#lib/pasted.ts';

	let view = $state<View>('all');
	let search = $state('');
	let sortKey = $state<SortKey>('queue');
	let sortDesc = $state(false);
	let adding = $state<{
		url: string;
		name?: string | null;
		headers?: Headers | null;
		title?: string | null;
		checksum?: string | null;
	} | null>(null);
	let picking = $state<CapturedLinks | null>(null);
	let grabbing = $state(false);
	let streaming = $state<CapturedMedia | null>(null);
	let pageMedia = $state<CapturedMedia | null>(null);
	let removing = $state<TaskId[] | null>(null);
	/** A queue about to start on a drive without room for it: what to do. */
	let diskAsk = $state<{ needs: DriveNeed[]; go: () => Promise<unknown> } | null>(null);
	let renaming = $state<Task | null>(null);
	/** "Where did this file come from?": what is known, or the dropped file's name. */
	let tracing = $state<{ origin: Origin | null; dropped: string | null } | null>(null);
	let menu = $state<{ x: number; y: number; items: Item[] } | null>(null);
	let notice = $state<string | null>(null);
	/** A passing message that is not an error. */
	let info = $state<string | null>(null);
	let settingsOpen = $state(false);
	let globalLimit = $state<number | null>(null);
	/** A newer version of the app, found by the daily check. */
	let update = $state<AppUpdate | null>(null);
	let updating = $state(false);
	/** What happens when all downloads finish (one shot). */
	let whenDone = $state<WhenDone>('nothing');
	let countdown = $state<{ action: WhenDone; seconds: number } | null>(null);
	/** The browser extensions heard from; its warning shows under the list. */
	let extensions = $state<ExtensionStatus | null>(null);
	/** The extension warning was closed for this run of the app. */
	let extensionLater = $state(false);
	const extensionWarning = $derived(extensionLater ? null : (extensions?.warning ?? null));
	const browserName = (b: string) => BROWSER_NAMES[b] ?? b;

	function loadExtensions() {
		extensionStatus()
			.then((s) => (extensions = s))
			.catch(() => {});
	}

	// The grace before "not found" runs out with the window open.
	$effect(() => {
		const secs = extensions?.check_in;
		if (secs == null) return;
		const timer = setTimeout(loadExtensions, (secs + 1) * 1000);
		return () => clearTimeout(timer);
	});

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
		loadExtensions();
		const unlisten = [
			onSettings((s) => (globalLimit = s.speed_limit)),
			onExtensions((s) => (extensions = s)),
			// The browser handed over a download: confirm it like IDM does.
			onCapture((c) => (adding = { url: c.url, name: c.filename, headers: c.headers, title: c.title, checksum: c.checksum })),
			onCapturedLinks((l) => (picking = l)),
			onNotice((text) => (info = text)),
			onUpdate((u) => (update = u)),
			// A copied link: offer it, unless the user is already adding something.
			onClipboard((url) => {
				if (!adding) adding = { url, name: null, headers: null };
			}),
			onWhenDone((a) => (whenDone = a)),
			onOriginPick(() => (tracing = { origin: null, dropped: null })),
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
	const detailed = $derived(selectedTasks.length === 1 ? selectedTasks[0] : null);
	const title = $derived(VIEWS.find((v) => v.id === view)?.label ?? '');
	const anyToStart = $derived(downloads.tasks.some(canStart));
	const anyToPause = $derived(downloads.tasks.some(canPause));

	function ids(pred: (t: Task) => boolean): TaskId[] {
		return selectedTasks.filter(pred).map((t) => t.id);
	}

	/** Runs an action, showing its error in the status bar instead of losing it. */
	/** Starts `ids` with `go`, first asking when a drive has not enough room for them. */
	async function startChecked(ids: TaskId[], go: () => Promise<unknown>) {
		const needs = await diskNeeds(ids).catch(() => [] as DriveNeed[]);
		const short = needs.filter((n) => n.short !== null && n.tasks.some((id) => ids.includes(id)));
		if (short.length === 0) return run(go());
		diskAsk = { needs: short, go };
	}

	/** Moves the downloads on the short drives (that are about to start) into `dir`. */
	async function moveThenStart(dir: string) {
		const ask = diskAsk;
		if (!ask) return;
		const moving = ask.needs.flatMap((n) => n.tasks);
		const starting = downloads.tasks
			.filter((t) => moving.includes(t.id) && t.status !== 'downloading')
			.map((t) => t.id);
		await run(
			(async () => {
				for (const id of starting) await relocateTask(id, dir);
				await ask.go();
			})()
		);
	}

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
		menu = {
			x: e.clientX,
			y: e.clientY,
			items: taskMenu(task, selectedTasks, {
				run,
				resume: () => startChecked(ids(canStart), () => startTasks(ids(canStart))),
				pause: () => run(pauseTasks(ids(canPause))),
				rename: (t) => (renaming = t),
				trace: (origin) => (tracing = { origin, dropped: null }),
				remove: (ids) => (removing = ids)
			})
		};
	}

	/** The download the list scrolls to. */
	let reveal = $state<TaskId | null>(null);

	/** Shows a download on the list, selected, and closes what asked for it. */
	function goTo(id: TaskId) {
		adding = null;
		picking = null;
		pageMedia = null;
		view = 'all';
		search = '';
		downloads.selected.clear();
		downloads.selected.add(id);
		reveal = id;
	}

	/** A pasted or dropped link: a page of a video site opens its own window (qualities, or a
	 * playlist's videos), anything else "New download". */
	function offer(url: string) {
		if (isVideoPage(url)) {
			pageMedia = {
				page: url,
				title: null,
				items: [{ url, kind: 'page', size: null, mime: null }],
				headers: []
			};
		} else {
			adding = { url };
		}
	}

	function keydown(e: KeyboardEvent) {
		if (adding || removing || renaming || settingsOpen || picking || streaming || pageMedia || tracing || inField(e))
			return;
		const ctrl = e.ctrlKey || e.metaKey;
		if (ctrl && e.key.toLowerCase() === 'a') {
			e.preventDefault();
			for (const t of visible) downloads.selected.add(t.id);
		} else if (ctrl && e.key.toLowerCase() === 'n') {
			e.preventDefault();
			adding = { url: '' };
		} else if (e.key === 'Delete' && selectedTasks.length) {
			removing = selectedTasks.map((t) => t.id);
		} else if (e.key === 'F2' && selectedTasks.length === 1) {
			e.preventDefault();
			renaming = selectedTasks[0];
		} else if (e.key === 'Enter' && selectedTasks.length === 1) {
			const t = selectedTasks[0];
			if (t.status === 'done') run(openFile(t.id));
		} else if (e.key === 'Escape') {
			downloads.selected.clear();
		}
	}

	function paste(e: ClipboardEvent) {
		if (adding || removing || renaming || settingsOpen || picking || inField(e)) return;
		const text = e.clipboardData?.getData('text/plain') ?? '';
		if (looksLikeUrl(text)) {
			e.preventDefault();
			offer(text.trim());
		}
	}

	/** A file dropped on the window: where it came from, by its size and content. */
	async function traceDropped(file: File) {
		try {
			const origin = await droppedFileOrigin(file.size, await sha256(file), file.name);
			tracing = { origin, dropped: origin ? null : file.name };
		} catch (err) {
			notice = String(err);
		}
	}

	function drop(e: DragEvent) {
		e.preventDefault();
		const file = e.dataTransfer?.files[0];
		if (file) {
			traceDropped(file);
			return;
		}
		const text =
			e.dataTransfer?.getData('text/uri-list').split('\n').find((l) => l && !l.startsWith('#')) ||
			e.dataTransfer?.getData('text/plain') ||
			'';
		if (looksLikeUrl(text)) offer(text.trim());
	}
</script>

<svelte:window onkeydown={keydown} onpaste={paste} ondragover={(e) => e.preventDefault()} ondrop={drop} />

<div class="app">
	<Sidebar
		bind:view
		limit={globalLimit}
		{whenDone}
		onadd={() => (adding = { url: '' })}
		ongrab={() => (grabbing = true)}
		onsettings={() => (settingsOpen = true)}
		onwhendone={(a) => run(setWhenDone(a))}
	/>

	<div class="main">
		<ListHeader
			{title}
			count={visible.length}
			selected={selectedTasks}
			{anyToStart}
			{anyToPause}
			bind:search
			onresume={() => startChecked(ids(canStart), () => startTasks(ids(canStart)))}
			onpause={() => run(pauseTasks(ids(canPause)))}
			onremove={() => (removing = selectedTasks.map((t) => t.id))}
			onclear={() => downloads.selected.clear()}
			onresumeall={() =>
				startChecked(
					downloads.tasks.filter((t) => t.status === 'paused' || t.status === 'error').map((t) => t.id),
					startAll
				)}
			onpauseall={() => run(pauseAll())}
		/>

		<main class:split={detailed}>
			<TaskList
				tasks={visible}
				bind:sortKey
				bind:sortDesc
				bind:reveal
				oncontext={contextMenu}
				onopen={openRow}
			/>
			{#if detailed}
				<DetailPane task={detailed} onerror={(m) => (notice = m)} onclose={() => downloads.selected.clear()} />
			{/if}
		</main>

		{#if downloads.diskLow && !notice}
			<div class="message error" role="alert">
				<span
					>{t('{n} downloads paused: {volume} has {free} left. Free some space or move them, then resume.', {
						n: downloads.diskLow.paused.length,
						volume: downloads.diskLow.volume,
						free: bytes(downloads.diskLow.free)
					})}</span
				>
				<Button variant="ghost" icon="close" onclick={() => (downloads.diskLow = null)} label={t('Dismiss')} />
			</div>
		{:else if notice || downloads.error}
			<div class="message error" role="alert">
				<span>{notice ?? downloads.error}</span>
				<Button variant="ghost" icon="close" onclick={() => (notice = null)} label={t('Dismiss')} />
			</div>
		{:else if update}
			<div class="message">
				<span
					>{t('Version {version} is ready.', { version: update.version })}
					<Text variant="muted" inline>{t('Running downloads continue after the restart.')}</Text></span
				>
				<Button variant="primary" onclick={applyUpdate} disabled={updating}
					>{updating ? t('Downloading…') : t('Restart and update')}</Button
				>
				<Button variant="ghost" icon="close" onclick={() => (update = null)} label={t('Later')} />
			</div>
		{:else if info}
			<div class="message">
				<span>{info}</span>
				<Button variant="ghost" icon="close" onclick={() => (info = null)} label={t('Dismiss')} />
			</div>
		{:else if extensionWarning?.kind === 'outdated'}
			<div class="message">
				<span
					>{t('The extension in {browser} is out of date.', { browser: browserName(extensionWarning.browser) })}
					<Text variant="muted" inline>{t('Update it so downloads keep reaching the app.')}</Text></span
				>
				<Button variant="primary" onclick={() => run(installExtension(extensionWarning.browser))}
					>{t('Update')}</Button
				>
				<Button variant="ghost" icon="close" onclick={() => (extensionLater = true)} label={t('Later')} />
			</div>
		{:else if extensionWarning?.kind === 'not_found'}
			<div class="message">
				<span
					>{t('Browser extension not found.')}
					<Text variant="muted" inline>{t('Install it to download from the browser with this app.')}</Text></span
				>
				<Button variant="primary" onclick={() => run(installExtension())}>{t('Install')}</Button>
				<Button
					variant="ghost"
					onclick={() => run(dismissExtensionWarning().then((s) => (extensions = s)))}
					>{t('Don’t show again')}</Button
				>
				<Button variant="ghost" icon="close" onclick={() => (extensionLater = true)} label={t('Later')} />
			</div>
		{/if}
	</div>
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
		title={adding.title}
		pageChecksum={adding.checksum}
		onclose={() => (adding = null)}
		onshow={goTo}
	/>
{/if}

{#if tracing}
	<OriginDialog origin={tracing.origin} dropped={tracing.dropped} onclose={() => (tracing = null)} />
{/if}

{#if diskAsk}
	<DiskDialog
		needs={diskAsk.needs}
		onmove={moveThenStart}
		onstart={() => diskAsk && run(diskAsk.go())}
		onclose={() => (diskAsk = null)}
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

{#if renaming}
	<RenameDialog task={renaming} onclose={() => (renaming = null)} />
{/if}

{#if pageMedia}
	<PageDialog media={pageMedia} onclose={() => (pageMedia = null)} onshow={goTo} />
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
		grid-template-columns: 224px 1fr;
		height: 100vh;
	}
	.main {
		display: grid;
		grid-template-rows: auto 1fr auto;
		min-width: 0;
		min-height: 0;
	}
	main {
		display: grid;
		grid-template-rows: 1fr;
		min-height: 0;
		min-width: 0;
	}
	main.split {
		grid-template-rows: 1fr 248px;
	}
	.message {
		display: flex;
		align-items: center;
		gap: var(--space-3);
		min-height: var(--space-10);
		padding: 0 var(--space-2) 0 var(--space-4);
		border-top: 1px solid var(--line-strong);
		background: var(--panel);
	}
	.message > span {
		flex: 1;
		min-width: 0;
	}
	.message.error {
		box-shadow: inset var(--space-1) 0 0 var(--danger);
		color: var(--danger);
	}
</style>
