<script lang="ts">
	// A stream the extension found: pick the quality, audio and subtitles.

	import { onMount } from 'svelte';
	import { inspectMedia, type CapturedMedia, type StreamInfo } from '../api.ts';
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
	let subtitles = $state(new Set<string>());
	let dialog: HTMLDialogElement | undefined = $state();

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
		dialog?.showModal();
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

	function toggleSubtitle(id: string) {
		const next = new Set(subtitles);
		if (next.has(id)) next.delete(id);
		else next.add(id);
		subtitles = next;
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
			<fieldset>
				<legend>Subtitles</legend>
				{#each info.subtitles as s (s.id ?? s.name)}
					<label class="option">
						<input type="checkbox" checked={subtitles.has(s.id ?? '')} onchange={() => toggleSubtitle(s.id ?? '')} />
						{trackName(s)}
					</label>
				{/each}
			</fieldset>
		{/if}
	{/if}

	<p class="note">
		Saving streams needs the stream downloader (N_m3u8DL-RE) and ffmpeg; the app will offer to set
		them up in the next step.
	</p>
	<div class="buttons">
		<button type="button" onclick={() => dialog?.close()}>Close</button>
		<button type="button" class="primary" disabled>Download</button>
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
	select {
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
