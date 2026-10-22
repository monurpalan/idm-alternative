<script lang="ts">
	// A page of a site such as YouTube: yt-dlp reads what it offers; pick a quality. A
	// playlist or a channel lists its videos to pick from, all at one quality.

	import { onMount } from 'svelte';
	import {
		addPlaylist,
		addTask,
		findPrevious,
		inspectPage,
		installTool,
		knownLinks,
		tools,
		ytdlpSelector,
		type CapturedMedia,
		type PageInfo,
		type Previous,
		type TaskId
	} from '../api.ts';
	import PreviousNotice from './PreviousNotice.svelte';
	import { duration } from '../format.ts';
	import { t } from '../i18n.svelte.ts';
	import { Button, Checkbox, Dialog, Field, Input, Section, Text } from './ui/index.ts';

	let {
		media,
		onclose,
		onshow
	}: { media: CapturedMedia; onclose: () => void; onshow: (id: TaskId) => void } = $props();
	/** An earlier download of this page. */
	let previous = $state<Previous | null>(null);

	const page = $derived(media.items[0]?.url ?? '');
	let info = $state<PageInfo | null>(null);
	let missing = $state(false);
	let loading = $state(false);
	let settingUp = $state(false);
	let error = $state<string | null>(null);
	/** A height, or 0 for audio alone. */
	let choice = $state(0);
	let name = $state('');
	let busy = $state(false);
	let dialog: { close(): void } | undefined = $state();

	const list = $derived(info?.playlist ?? null);
	/** Per video of the list: chosen, and downloaded before (or on the list). */
	let chosen = $state<boolean[]>([]);
	let known = $state<boolean[]>([]);
	let filter = $state('');
	let firstN = $state('');
	/** Indices of the videos the filter shows. */
	const shown = $derived.by(() => {
		const words = filter.toLowerCase().split(/\s+/).filter(Boolean);
		return (list?.entries ?? []).flatMap((e, i) =>
			words.every((w) => (e.title ?? '').toLowerCase().includes(w)) ? [i] : []
		);
	});
	const count = $derived(chosen.filter(Boolean).length);

	async function load(wholeList = false) {
		loading = true;
		error = null;
		try {
			const found = await tools();
			missing = !found.ytdlp;
			if (missing) return;
			info = await inspectPage(page, media.headers, wholeList);
			choice = info.playlist ? 1080 : (info.heights[0] ?? 0);
			name = info.playlist?.title ?? info.title ?? media.title ?? '';
			if (info.playlist) {
				const urls = info.playlist.entries.map((e) => e.url);
				known = await knownLinks(urls).catch(() => urls.map(() => false));
				// Ones downloaded before start unticked.
				chosen = known.map((k) => !k);
			}
		} catch (e) {
			error = String(e);
		} finally {
			loading = false;
		}
	}

	async function setUp() {
		settingUp = true;
		error = null;
		try {
			await installTool('ytdlp');
			await installTool('deno');
			await load();
		} catch (e) {
			error = String(e);
		} finally {
			settingUp = false;
		}
	}

	function pickFirst() {
		const n = Math.max(0, Math.floor(Number(firstN)) || 0);
		chosen = chosen.map((_, i) => i < n);
	}

	async function download() {
		busy = true;
		try {
			if (list) {
				await addPlaylist({
					title: name.trim() || list.title || t('Playlist'),
					entries: list.entries
						.filter((_, i) => chosen[i])
						.map((e) => ({ url: e.url, title: e.title })),
					height: choice || null,
					headers: media.headers,
					start: true
				});
			} else {
				await addTask({
					url: page,
					name: name.trim() || null,
					dir: null,
					start: true,
					login: null,
					headers: media.headers,
					stream: { audio: null, ytdlp: ytdlpSelector(choice || null) }
				});
			}
			dialog?.close();
		} catch (e) {
			error = String(e);
			busy = false;
		}
	}

	onMount(() => {
		findPrevious(page)
			.then((p) => (previous = p))
			.catch(() => {});
		load();
	});
</script>

<Dialog
	bind:this={dialog}
	title={list?.title || info?.title || media.title || t('Video')}
	size={list ? 'wide' : 'normal'}
	tall={!!list}
	{onclose}
>
	<Text variant="muted" truncate title={page}>{page}</Text>

	{#if missing}
		<Text>{t('Downloading from this site needs yt-dlp (and deno, which it uses for YouTube).')}</Text>
		<Text variant="caption">
			{t('Both are downloaded from their official GitHub releases and checked against their published SHA-256.')}
		</Text>
	{:else if loading}
		<Text variant="muted">{t('yt-dlp is reading the page…')}</Text>
	{:else if error}
		<Text variant="error">{error}</Text>
	{:else if info && list}
		<Text variant="muted">
			{list.channel ? t('Channel, newest first') : t('Playlist')}{list.uploader ? ` · ${list.uploader}` : ''} ·
			{t('{n} videos', { n: list.entries.length })}
		</Text>
		<div class="bar">
			<Input variant="search" bind:value={filter} placeholder={t('Filter by name')} label={t('Filter by name')} />
			<Button onclick={() => (chosen = chosen.map((c, i) => c || shown.includes(i)))}>{t('All')}</Button>
			<Button onclick={() => (chosen = chosen.map(() => false))}>{t('None')}</Button>
			<span class="first">
				<Input inputmode="numeric" bind:value={firstN} placeholder="10" label={t('How many from the top')} />
				<Button onclick={pickFirst} disabled={!firstN.trim()}>{list.channel ? t('Newest') : t('First')}</Button>
			</span>
		</div>
		<ul class="videos">
			{#each shown as i (i)}
				{@const e = list.entries[i]}
				<li>
					<Checkbox bind:checked={chosen[i]}>
						<span class="video" title={e.url}>
							{#if e.thumbnail}<img src={e.thumbnail} alt="" loading="lazy" />{:else}<span class="thumb"></span>{/if}
							<span class="vtitle">{e.title ?? e.url}</span>
							{#if known[i]}<span class="muted">{t('downloaded before')}</span>{/if}
							<span class="muted num">{duration(e.duration)}</span>
						</span>
					</Checkbox>
				</li>
			{:else}
				<li><Text variant="muted">{t('No videos match.')}</Text></li>
			{/each}
		</ul>
		<Section title={t('Quality, for each')}>
			<div class="qualities">
				{#each info.heights as h (h)}
					<Checkbox type="radio" name="q" value={h} bind:group={choice}>{h}p</Checkbox>
				{/each}
				<Checkbox type="radio" name="q" value={0} bind:group={choice}>{t('Audio only')}</Checkbox>
			</div>
		</Section>
		<Field label={t('Folder name')}>
			<Input bind:value={name} placeholder={t('Name of the list')} />
		</Field>
	{:else if info}
		{#if info.duration}<Text variant="muted">{duration(info.duration)}</Text>{/if}
		<Section title={t('Quality')}>
			{#each info.heights as h (h)}
				<Checkbox type="radio" name="q" value={h} bind:group={choice}>{h}p</Checkbox>
			{/each}
			{#if info.audio_only}
				<Checkbox type="radio" name="q" value={0} bind:group={choice}>{t('Audio only')}</Checkbox>
			{/if}
		</Section>
		<Field label={t('Save as')}>
			<Input bind:value={name} placeholder={t('Title of the page')} />
		</Field>
		{#if info.in_playlist}
			<div>
				<Button variant="link" onclick={() => load(true)}>{t('This video is in a playlist: show the whole list…')}</Button>
			</div>
		{/if}
	{/if}
	{#if previous && !list}
		<PreviousNotice {previous} {onshow} onerror={(e) => (error = e)} />
	{/if}

	{#snippet actions()}
		<Button onclick={() => dialog?.close()}>{t('Close')}</Button>
		{#if missing}
			<Button variant="primary" disabled={settingUp} onclick={setUp}>
				{settingUp ? t('Setting up…') : t('Set up yt-dlp')}
			</Button>
		{:else if list}
			<Button variant="primary" disabled={!count || busy} onclick={download}
				>{t('Download {n}', { n: count })}</Button
			>
		{:else}
			<Button variant="primary" disabled={!info || busy} onclick={download}
				>{previous ? t('Download anyway') : t('Download')}</Button
			>
		{/if}
	{/snippet}
</Dialog>

<style>
	.bar {
		display: flex;
		align-items: center;
		gap: var(--space-2);
	}
	.first {
		display: flex;
		gap: var(--space-1);
		width: 160px;
		margin-left: auto;
	}
	.videos {
		flex: 1;
		min-height: 0;
		margin: 0;
		padding: 0;
		overflow-y: auto;
		border: 1px solid var(--line);
		border-radius: var(--radius-1);
		list-style: none;
	}
	.videos li {
		padding: var(--space-1) var(--space-2);
		border-bottom: 1px solid var(--line-soft);
	}
	.video {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		min-width: 0;
	}
	img,
	.thumb {
		flex: none;
		width: 64px;
		height: 36px;
		border-radius: var(--radius-1);
		background: var(--track);
		object-fit: cover;
	}
	.vtitle {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.muted {
		flex: none;
		color: var(--muted);
	}
	.num {
		font-family: var(--font-mono);
		font-size: var(--text-caption);
	}
	.qualities {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2) var(--space-4);
	}
</style>
