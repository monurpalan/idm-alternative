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
	let dialog: HTMLDialogElement | undefined = $state();

	async function load() {
		loading = true;
		error = null;
		try {
			const t = await tools();
			missing = !t.ytdlp;
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

	onMount(() => {
		dialog?.showModal();
		load();
	});
</script>

<dialog bind:this={dialog} onclose={onclose} aria-labelledby="page-title">
	<h2 id="page-title">{info?.title || media.title || 'Video'}</h2>
	<p class="muted url" title={page}>{page}</p>

	{#if missing}
		<p>Downloading from this site needs yt-dlp (and deno, which it uses for YouTube).</p>
		<p class="muted small">
			Both are downloaded from their official GitHub releases and checked against their published SHA-256.
		</p>
		<div class="buttons">
			<button type="button" onclick={() => dialog?.close()}>Close</button>
			<button type="button" class="primary" disabled={settingUp} onclick={setUp}>
				{settingUp ? 'Setting up…' : 'Set up yt-dlp'}
			</button>
		</div>
	{:else}
		{#if loading}
			<p class="muted">yt-dlp is reading the page…</p>
		{:else if error}
			<p class="error">{error}</p>
		{:else if info}
			{#if info.duration}<p class="muted">{duration(info.duration)}</p>{/if}
			<fieldset>
				<legend>Quality</legend>
				{#each info.heights as h (h)}
					<label class="option"><input type="radio" name="q" value={h} bind:group={choice} /> {h}p</label>
				{/each}
				{#if info.audio_only}
					<label class="option"><input type="radio" name="q" value={0} bind:group={choice} /> Audio only</label>
				{/if}
			</fieldset>
			<label class="field">
				Save as
				<input bind:value={name} placeholder="Title of the page" />
			</label>
		{/if}
		<div class="buttons">
			<button type="button" onclick={() => dialog?.close()}>Close</button>
			<button type="button" class="primary" disabled={!info || busy} onclick={download}>Download</button>
		</div>
	{/if}
</dialog>

<style>
	dialog {
		width: min(520px, calc(100vw - 32px));
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
		margin: 0 0 4px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.url {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		margin-top: 0;
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
	}
	.field {
		display: flex;
		flex-direction: column;
		gap: 4px;
		color: var(--muted);
	}
	.field input {
		padding: 5px 8px;
		border: 1px solid var(--line);
		border-radius: 5px;
		background: var(--field);
		color: var(--fg);
		font: inherit;
	}
	.muted {
		color: var(--muted);
	}
	.small {
		font-size: 12px;
	}
	.error {
		color: var(--danger);
	}
	.buttons {
		display: flex;
		justify-content: flex-end;
		gap: 8px;
		margin-top: 14px;
	}
</style>
