<script lang="ts">
	// "Download all links": the links of a page, to pick from. Filtering by type keeps a page
	// of hundreds of links manageable.

	import { addTask, knownLinks, newGroup } from '../api.ts';
	import { nameFromUrl } from '../downloads.svelte.ts';
	import { t } from '../i18n.svelte.ts';
	import { Button, Checkbox, Dialog, Input, Text } from './ui/index.ts';

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
	let dialog: { close(): void } | undefined = $state();

	/** Typing ".zip .pdf" keeps those types; any other text matches names and link texts. */
	const shown = $derived.by(() => {
		const words = filter.toLowerCase().split(/\s+/).filter(Boolean);
		if (!words.length) return items;
		return items.filter((i) => {
			const hay = `${i.name} ${i.text} ${i.url}`.toLowerCase();
			return words.some((w) => (w.startsWith('.') ? i.name.toLowerCase().endsWith(w) : hay.includes(w)));
		});
	});

	// Another choice is asked about afresh.
	$effect(() => {
		void chosen;
		known = null;
	});

	function toggle(url: string) {
		const next = new Set(chosen);
		if (next.has(url)) next.delete(url);
		else next.add(url);
		chosen = next;
	}

	/** Of the chosen links, those downloaded before or on the list; asked once, on the
	 * first "Download". */
	let known = $state<string[] | null>(null);

	/** With `skip`, leaves out the links downloaded before. */
	async function download(skip = false) {
		busy = true;
		const headers: [string, string][] = referrer ? [['Referer', referrer]] : [];
		try {
			const picked = items.filter((i) => chosen.has(i.url));
			if (known === null) {
				const flags = await knownLinks(picked.map((i) => i.url));
				const found = picked.filter((_, n) => flags[n]).map((i) => i.url);
				if (found.length) {
					known = found;
					busy = false;
					return;
				}
			}
			const adding = picked.filter((i) => !(skip && known?.includes(i.url)));
			// Added together: one group, named after the page they came from.
			let group: number | null = null;
			if (adding.length > 1) {
				let site = referrer ?? '';
				try {
					site = referrer ? new URL(referrer).hostname + new URL(referrer).pathname : '';
				} catch {
					// not an address: keep it as it is
				}
				group = (await newGroup(site.replace(/\/$/, '') || t('Links'))).id;
			}
			for (const i of adding) {
				await addTask({ url: i.url, name: null, dir: null, start: true, login: null, headers, group });
			}
			onerror(null);
			dialog?.close();
		} catch (e) {
			onerror(String(e));
			busy = false;
		}
	}
</script>

<Dialog bind:this={dialog} title={t('Links on the page')} size="wide" tall {onclose}>
	<div class="bar">
		<Input
			variant="search"
			bind:value={filter}
			placeholder={t('Filter: .zip .pdf, or any text')}
			label={t('Filter')}
		/>
		<Button onclick={() => (chosen = new Set([...chosen, ...shown.map((i) => i.url)]))}
			>{t('Select shown')}</Button
		>
		<Button onclick={() => (chosen = new Set())}>{t('None')}</Button>
	</div>
	<ul>
		{#each shown as i (i.url)}
			<li title={i.url}>
				<Checkbox checked={chosen.has(i.url)} onchange={() => toggle(i.url)}>
					{i.name}
					{#if i.text && i.text !== i.name}<span class="text">{i.text}</span>{/if}
				</Checkbox>
			</li>
		{:else}
			<li class="empty"><Text variant="muted">{t('No links match.')}</Text></li>
		{/each}
	</ul>

	{#snippet actions()}
		{#if known}
			<span class="count">
				<Text>{t('{n} of the {total} were downloaded before or are on the list.', { n: known.length, total: chosen.size })}</Text>
			</span>
			<Button onclick={() => download(false)} disabled={busy}>{t('Download anyway')}</Button>
			<Button variant="primary" onclick={() => download(true)} disabled={busy || known.length === chosen.size}
				>{t('Skip them')}</Button
			>
		{:else}
			<span class="count">
				<Text variant="muted">{t('{n} of {total} selected', { n: chosen.size, total: items.length })}</Text>
			</span>
			<Button onclick={() => dialog?.close()}>{t('Cancel')}</Button>
			<Button variant="primary" disabled={!chosen.size || busy} onclick={() => download()}>
				{chosen.size ? t('Download {n}', { n: chosen.size }) : t('Download')}
			</Button>
		{/if}
	{/snippet}
</Dialog>

<style>
	.bar {
		display: flex;
		gap: var(--space-2);
	}
	ul {
		flex: 1;
		min-height: 0;
		margin: 0;
		padding: 0;
		overflow-y: auto;
		border: 1px solid var(--line);
		border-radius: var(--radius-1);
		list-style: none;
	}
	li {
		padding: var(--space-1) var(--space-2);
		overflow: hidden;
		border-bottom: 1px solid var(--line-soft);
		white-space: nowrap;
	}
	li:hover {
		background: var(--hover);
	}
	.text {
		margin-left: var(--space-2);
		color: var(--muted);
	}
	.empty {
		padding: var(--space-4);
		text-align: center;
	}
	.empty:hover {
		background: transparent;
	}
	.count {
		margin-right: auto;
	}
</style>
