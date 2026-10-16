<script lang="ts">
	// "Download all links": the links of a page, to pick from. Filtering by type keeps a page
	// of hundreds of links manageable.

	import { onMount } from 'svelte';
	import { addTask } from '../api.ts';
	import { nameFromUrl } from '../downloads.svelte.ts';

	let {
		referrer,
		links,
		onclose,
		onerror
	}: {
		referrer: string | null;
		links: { url: string; text: string }[];
		onclose: () => void;
		onerror: (message: string | null) => void;
	} = $props();

	/** Each link once, with the name it would get. */
	const items = $derived.by(() => {
		const seen = new Set<string>();
		return links
			.filter((l) => /^(https?|ftp):/i.test(l.url) && !seen.has(l.url) && seen.add(l.url))
			.map((l) => ({ ...l, name: nameFromUrl(l.url) }));
	});

	let filter = $state('');
	let chosen = $state(new Set<string>());
	let busy = $state(false);
	let dialog: HTMLDialogElement | undefined = $state();

	/** Typing ".zip .pdf" keeps those types; any other text matches names and link texts. */
	const shown = $derived.by(() => {
		const words = filter.toLowerCase().split(/\s+/).filter(Boolean);
		if (!words.length) return items;
		return items.filter((i) => {
			const hay = `${i.name} ${i.text} ${i.url}`.toLowerCase();
			return words.some((w) => (w.startsWith('.') ? i.name.toLowerCase().endsWith(w) : hay.includes(w)));
		});
	});

	onMount(() => dialog?.showModal());

	function toggle(url: string) {
		const next = new Set(chosen);
		if (next.has(url)) next.delete(url);
		else next.add(url);
		chosen = next;
	}

	async function download() {
		busy = true;
		const headers: [string, string][] = referrer ? [['Referer', referrer]] : [];
		try {
			for (const i of items.filter((i) => chosen.has(i.url))) {
				await addTask({ url: i.url, name: null, dir: null, start: true, login: null, headers });
			}
			onerror(null);
			dialog?.close();
		} catch (e) {
			onerror(String(e));
			busy = false;
		}
	}
</script>

<dialog bind:this={dialog} onclose={onclose} aria-labelledby="links-title">
	<h2 id="links-title">Links on the page</h2>
	<div class="bar">
		<input bind:value={filter} placeholder="Filter: .zip .pdf, or any text" aria-label="Filter" />
		<button type="button" onclick={() => (chosen = new Set([...chosen, ...shown.map((i) => i.url)]))}
			>Select shown</button
		>
		<button type="button" onclick={() => (chosen = new Set())}>None</button>
	</div>
	<ul>
		{#each shown as i (i.url)}
			<li>
				<label title={i.url}>
					<input type="checkbox" checked={chosen.has(i.url)} onchange={() => toggle(i.url)} />
					<span class="name">{i.name}</span>
					{#if i.text && i.text !== i.name}<span class="text">{i.text}</span>{/if}
				</label>
			</li>
		{:else}
			<li class="empty">No links match.</li>
		{/each}
	</ul>
	<div class="buttons">
		<span class="count">{chosen.size} of {items.length} selected</span>
		<button type="button" onclick={() => dialog?.close()}>Cancel</button>
		<button type="button" class="primary" disabled={!chosen.size || busy} onclick={download}>
			Download {chosen.size || ''}
		</button>
	</div>
</dialog>

<style>
	dialog {
		width: min(680px, calc(100vw - 32px));
		height: min(560px, calc(100vh - 48px));
		border: 1px solid var(--line);
		border-radius: 10px;
		background: var(--bg);
		color: var(--fg);
		padding: 18px 20px;
		box-shadow: 0 12px 40px rgb(0 0 0 / 0.25);
		box-sizing: border-box;
	}
	dialog[open] {
		display: flex;
		flex-direction: column;
	}
	dialog::backdrop {
		background: rgb(0 0 0 / 0.3);
	}
	h2 {
		font-size: 16px;
		margin: 0 0 12px;
	}
	.bar {
		display: flex;
		gap: 6px;
		margin-bottom: 8px;
	}
	.bar input {
		flex: 1;
		padding: 5px 8px;
		border: 1px solid var(--line);
		border-radius: 5px;
		background: var(--field);
		color: var(--fg);
		font: inherit;
	}
	ul {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		list-style: none;
		margin: 0;
		padding: 0;
		border: 1px solid var(--line);
		border-radius: 6px;
	}
	li label {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 5px 8px;
		border-bottom: 1px solid var(--line-soft);
		white-space: nowrap;
		overflow: hidden;
		cursor: default;
	}
	li label:hover {
		background: var(--hover);
	}
	.name {
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.text {
		color: var(--muted);
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.empty {
		color: var(--muted);
		padding: 16px;
		text-align: center;
	}
	.buttons {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: 8px;
		margin-top: 12px;
	}
	.count {
		margin-right: auto;
		color: var(--muted);
	}
</style>
