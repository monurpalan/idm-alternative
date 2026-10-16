<script lang="ts">
	// A stream the extension found: pick the quality, audio and subtitles.

	import { onMount } from 'svelte';
	import { addTask, inspectMedia, tools, type CapturedMedia, type StreamInfo } from '../api.ts';
	import { duration } from '../format.ts';

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
	let ffmpeg = $state(true);
	let busy = $state(false);
	let dialog: HTMLDialogElement | undefined = $state();

	/** Why the stream cannot be downloaded, if it cannot. */
	const blocked = $derived.by(() => {
		if (!info) return 'Read the playlist first';
		if (info.encryption === 'DRM') return 'Protected streams cannot be downloaded';
		if (info.live) return 'Live streams cannot be downloaded yet';
		if (!ffmpeg && info.audio.length) return 'This stream has separate audio: ffmpeg is needed';
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
				stream:
					info.format === 'dash'
						? { audio: audio || null, dash: video }
						: { audio: audio || null }
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
		} catch (e) {
			error = String(e);
		} finally {
			loading = false;
		}
	}

	onMount(() => {
		dialog?.showModal();
		name = media.title ?? '';
		tools()
			.then((t) => (ffmpeg = t.ffmpeg))
			.catch(() => {});
		if (streams[0]) load(streams[0].url);
	});

	function quality(v: StreamInfo['video'][number]): string {
		const parts = [];
		if (v.height) parts.push(`${v.height}p`);
		if (v.frame_rate && v.frame_rate > 30.5) parts.push(`${Math.round(v.frame_rate)} fps`);
		if (v.bandwidth) parts.push(`${(v.bandwidth / 1e6).toFixed(1)} Mbps`);
		if (v.codecs) parts.push(v.codecs.split(',')[0]);
		return parts.join(' · ') || 'As offered';
	}

	function trackName(t: StreamInfo['audio'][number]): string {
		return [t.name, t.language && t.language !== t.name ? `(${t.language})` : ''].filter(Boolean).join(' ') || (t.id ?? 'Track');
	}

</script>

<dialog bind:this={dialog} onclose={onclose} aria-labelledby="media-title">
	<h2 id="media-title">{media.title || 'Video'}</h2>
	{#if streams.length > 1}
		<label class="field">
			Stream
			<select value={chosenUrl} onchange={(e) => load((e.target as HTMLSelectElement).value)}>
				{#each streams as s (s.url)}
					<option value={s.url}>{s.kind.toUpperCase()} · {s.url}</option>
				{/each}
			</select>
		</label>
	{/if}

	{#if loading}
		<p class="muted">Reading the playlist…</p>
	{:else if error}
		<p class="error">{error}</p>
	{:else if info}
		<p class="muted">
			{info.format.toUpperCase()}{info.duration ? ` · ${duration(info.duration)}` : ''}{info.live
				? ' · live'
				: ''}
		</p>
		{#if info.encryption === 'DRM'}
			<p class="error">This stream is protected (DRM). It cannot be saved in a playable form.</p>
		{:else if info.live}
			<p class="warn">A live stream: downloading records it until you stop it.</p>
		{/if}

		<fieldset>
			<legend>Quality</legend>
			{#each info.video as v (v.id)}
				<label class="option">
					<input type="radio" name="video" value={v.id} bind:group={video} />
					{quality(v)}
				</label>
			{/each}
		</fieldset>

		{#if info.audio.length}
			<label class="field">
				Audio
				<select bind:value={audio}>
					{#each info.audio as a (a.id ?? a.name)}
						<option value={a.id ?? ''}>{trackName(a)}</option>
					{/each}
				</select>
			</label>
		{/if}

		{#if info.subtitles.length}
			<p class="note">
				Subtitles offered ({info.subtitles.map(trackName).join(', ')}) are not saved yet.
			</p>
		{/if}

		<label class="field">
			Save as
			<input bind:value={name} placeholder="Name from the address" />
		</label>
	{/if}

	{#if info && !ffmpeg}
		<p class="note">ffmpeg was not found: the video is saved as joined .ts segments.</p>
	{/if}
	{#if blocked && info}<p class="note">{blocked}.</p>{/if}
	<div class="buttons">
		<button type="button" onclick={() => dialog?.close()}>Close</button>
		<button type="button" class="primary" disabled={!!blocked || busy} onclick={download}>Download</button>
	</div>
</dialog>

<style>
	dialog {
		width: min(560px, calc(100vw - 32px));
		max-height: calc(100vh - 48px);
		border: 1px solid var(--line);
		border-radius: 10px;
		background: var(--bg);
		color: var(--fg);
		padding: 18px 20px;
		box-shadow: 0 12px 40px rgb(0 0 0 / 0.25);
	}
	dialog::backdrop {
		background: rgb(0 0 0 / 0.3);
	}
	h2 {
		font-size: 16px;
		margin: 0 0 8px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	fieldset {
		border: 1px solid var(--line);
		border-radius: 8px;
		margin: 10px 0;
		padding: 6px 12px 8px;
	}
	legend {
		color: var(--muted);
		padding: 0 4px;
	}
	.option {
		display: flex;
		gap: 8px;
		align-items: center;
		padding: 3px 0;
		font-variant-numeric: tabular-nums;
	}
	.field {
		display: flex;
		flex-direction: column;
		gap: 4px;
		color: var(--muted);
		margin: 10px 0;
	}
	select,
	.field input {
		padding: 5px 8px;
		border: 1px solid var(--line);
		border-radius: 5px;
		background: var(--field);
		color: var(--fg);
		font: inherit;
	}
	.muted,
	.note {
		color: var(--muted);
	}
	.note {
		font-size: 12px;
	}
	.error {
		color: var(--danger);
	}
	.warn {
		color: var(--fg);
	}
	.buttons {
		display: flex;
		justify-content: flex-end;
		gap: 8px;
		margin-top: 12px;
	}
</style>
