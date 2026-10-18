<script lang="ts">
	// A page of a site such as YouTube: yt-dlp reads what it offers; pick a quality.

	import { onMount } from 'svelte';
	import {
		addTask,
		inspectPage,
		installTool,
		tools,
		ytdlpSelector,
		type CapturedMedia,
		type PageInfo
	} from '../api.ts';
	import { duration } from '../format.ts';
	import { t } from '../i18n.svelte.ts';
	import { Button, Checkbox, Dialog, Field, Input, Section, Text } from './ui/index.ts';

	let { media, onclose }: { media: CapturedMedia; onclose: () => void } = $props();

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

	async function load() {
		loading = true;
		error = null;
		try {
			const found = await tools();
			missing = !found.ytdlp;
			if (missing) return;
			info = await inspectPage(page, media.headers);
			choice = info.heights[0] ?? 0;
			name = info.title ?? media.title ?? '';
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

	async function download() {
		busy = true;
		try {
			await addTask({
				url: page,
				name: name.trim() || null,
				dir: null,
				start: true,
				login: null,
				headers: media.headers,
				stream: { audio: null, ytdlp: ytdlpSelector(choice || null) }
			});
			dialog?.close();
		} catch (e) {
			error = String(e);
			busy = false;
		}
	}

	onMount(load);
</script>

<Dialog bind:this={dialog} title={info?.title || media.title || t('Video')} {onclose}>
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
	{/if}

	{#snippet actions()}
		<Button onclick={() => dialog?.close()}>{t('Close')}</Button>
		{#if missing}
			<Button variant="primary" disabled={settingUp} onclick={setUp}>
				{settingUp ? t('Setting up…') : t('Set up yt-dlp')}
			</Button>
		{:else}
			<Button variant="primary" disabled={!info || busy} onclick={download}>{t('Download')}</Button>
		{/if}
	{/snippet}
</Dialog>
