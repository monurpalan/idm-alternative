<script lang="ts">
	// A stream the extension found: pick the quality, audio and subtitles.

	import { onMount } from 'svelte';
	import {
		addTask,
		inspectMedia,
		tools,
		type CapturedMedia,
		type Clip,
		type Recording,
		type StreamInfo
	} from '../api.ts';
	import ClipFields from './ClipFields.svelte';
	import RecordFields from './RecordFields.svelte';
	import { duration } from '../format.ts';
	import { msg, t } from '../i18n.svelte.ts';
	import { Button, Checkbox, Dialog, Field, Input, Section, Select, Text } from './ui/index.ts';

	let {
		media,
		onclose
	}: {
		media: CapturedMedia;
		onclose: () => void;
	} = $props();

	const streams = $derived(media.items.filter((i) => i.kind === 'hls' || i.kind === 'dash'));
	let chosenUrl = $state('');
	let info = $state<StreamInfo | null>(null);
	let error = $state<string | null>(null);
	let loading = $state(false);
	let video = $state('');
	let audio = $state('');
	let name = $state('');
	/** Ids of the subtitle tracks to save. */
	let subtitles = $state(new Set<string>());
	let ffmpeg = $state(true);
	let busy = $state(false);
	let dialog: { close(): void } | undefined = $state();
	let clip = $state<Clip | null>(null);
	let clipProblem = $state<string | null>(null);
	let record = $state<Recording>({ limit: null, from_start: false, start_at: null });
	let recordProblem = $state<string | null>(null);

	/** Why the stream cannot be downloaded, if it cannot. */
	const blocked = $derived.by(() => {
		if (!info) return msg('Read the playlist first');
		if (info.encryption === 'DRM') return msg('Protected streams cannot be downloaded');
		if (info.live && info.format === 'dash') return msg('Live DASH streams cannot be recorded yet');
		return null;
	});

	async function download() {
		if (!info || blocked) return;
		busy = true;
		try {
			await addTask({
				// HLS: the chosen variant's playlist; DASH: the manifest, with representation ids.
				url: info.format === 'dash' ? chosenUrl : video,
				name: name.trim() || null,
				dir: null,
				start: true,
				login: null,
				headers: media.headers,
				stream: {
					audio: audio || null,
					dash: info.format === 'dash' ? video : null,
					subtitles: info.subtitles
						.filter((s) => s.id && subtitles.has(s.id))
						.map((s) => ({ id: s.id!, language: s.language })),
					clip: info.live ? null : clip,
					record: info.live ? record : null
				}
			});
			dialog?.close();
		} catch (e) {
			error = String(e);
			busy = false;
		}
	}

	async function load(url: string) {
		chosenUrl = url;
		loading = true;
		error = null;
		info = null;
		try {
			info = await inspectMedia(url, media.headers);
			video = info.video[0]?.id ?? '';
			audio = (info.audio.find((a) => a.default) ?? info.audio[0])?.id ?? '';
			subtitles = new Set();
		} catch (e) {
			error = String(e);
		} finally {
			loading = false;
		}
	}

	onMount(() => {
		name = media.title ?? '';
		tools()
			.then((found) => (ffmpeg = found.ffmpeg !== null))
			.catch(() => {});
		if (streams[0]) load(streams[0].url);
	});

	function quality(v: StreamInfo['video'][number]): string {
		const parts = [];
		if (v.height) parts.push(`${v.height}p`);
		if (v.frame_rate && v.frame_rate > 30.5) parts.push(`${Math.round(v.frame_rate)} fps`);
		if (v.bandwidth) parts.push(`${(v.bandwidth / 1e6).toFixed(1)} Mbps`);
		if (v.codecs) parts.push(v.codecs.split(',')[0]);
		return parts.join(' · ') || t('As offered');
	}

	function trackName(track: StreamInfo['audio'][number]): string {
		const lang = track.language && track.language !== track.name ? `(${track.language})` : '';
		return [track.name, lang].filter(Boolean).join(' ') || (track.id ?? t('Track'));
	}
</script>

<Dialog bind:this={dialog} title={media.title || t('Video')} {onclose}>
	{#if streams.length > 1}
		<Field label={t('Stream')}>
			<Select value={chosenUrl} onchange={(e) => load(e.currentTarget.value)}>
				{#each streams as s (s.url)}
					<option value={s.url}>{s.kind.toUpperCase()} · {s.url}</option>
				{/each}
			</Select>
		</Field>
	{/if}

	{#if loading}
		<Text variant="muted">{t('Reading the playlist…')}</Text>
	{:else if error}
		<Text variant="error">{error}</Text>
	{:else if info}
		<Text variant="muted">
			{info.format.toUpperCase()}{info.duration ? ` · ${duration(info.duration)}` : ''}{info.live
				? ` · ${t('live')}`
				: ''}
		</Text>
		{#if info.encryption === 'DRM'}
			<Text variant="error">{t('This stream is protected (DRM). It cannot be saved in a playable form.')}</Text>
		{/if}

		<Section title={t('Quality')}>
			{#each info.video as v (v.id)}
				<Checkbox type="radio" name="video" value={v.id} bind:group={video}>{quality(v)}</Checkbox>
			{/each}
		</Section>

		{#if info.audio.length}
			<Field label={t('Audio')}>
				<Select bind:value={audio}>
					{#each info.audio as a (a.id ?? a.name)}
						<option value={a.id ?? ''}>{trackName(a)}</option>
					{/each}
				</Select>
			</Field>
		{/if}

		{#if info.subtitles.length}
			<Section title={t('Subtitles (saved next to the video)')}>
				{#each info.subtitles as s (s.id ?? s.name)}
					<Checkbox
						disabled={!s.id}
						checked={!!s.id && subtitles.has(s.id)}
						onchange={() => {
							const next = new Set(subtitles);
							if (s.id && next.has(s.id)) next.delete(s.id);
							else if (s.id) next.add(s.id);
							subtitles = next;
						}}>{trackName(s)}</Checkbox
					>
				{/each}
			</Section>
		{/if}

		<Field label={t('Save as')}>
			<Input bind:value={name} placeholder={t('Name from the address')} />
		</Field>
		{#if info.live}
			<RecordFields bind:record bind:problem={recordProblem} />
		{:else}
			<ClipFields length={info.duration} bind:clip bind:problem={clipProblem} />
		{/if}
	{/if}

	{#if info && !ffmpeg && info.audio.length}
		<Text variant="caption">{t('ffmpeg, which joins the video and its audio, is set up when the download starts.')}</Text>
	{:else if info && !ffmpeg && info.format !== 'dash'}
		<Text variant="caption">{t('Without ffmpeg the video is saved as joined .ts segments.')}</Text>
	{/if}
	{#if blocked && info}<Text variant="caption">{t(blocked)}.</Text>{/if}

	{#snippet actions()}
		<Button onclick={() => dialog?.close()}>{t('Close')}</Button>
		<Button
			variant="primary"
			disabled={!!blocked || busy || !!(info?.live ? recordProblem : clipProblem)}
			onclick={download}>{info?.live ? t('Record') : t('Download')}</Button
		>
	{/snippet}
</Dialog>
