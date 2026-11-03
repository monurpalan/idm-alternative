<script lang="ts">
	// A file its server no longer has (404, 410): the Wayback Machine's copies of the address,
	// in the browser, or its closest copy added as a download. Nothing is asked until a click.

	import { Button } from './ui/index.ts';
	import { addArchivedCopy, searchWayback, type TaskId } from '../api.ts';
	import { t } from '../i18n.svelte.ts';

	let { id, onerror }: { id: TaskId; onerror: (message: string | null) => void } = $props();

	let asking = $state(false);
	/** The answer: when the copy added was archived, `''` for none; said in the language now. */
	let archived = $state<string | null>(null);

	/** `20240131120000` → `2024-01-31`. */
	const day = (ts: string) => `${ts.slice(0, 4)}-${ts.slice(4, 6)}-${ts.slice(6, 8)}`;

	async function addCopy() {
		asking = true;
		try {
			const copy = await addArchivedCopy(id);
			archived = copy ? copy.timestamp : '';
			onerror(null);
		} catch (err) {
			onerror(String(err));
		} finally {
			asking = false;
		}
	}
</script>

<dt>{t('Archive')}</dt>
<dd>
	<Button variant="link" onclick={() => searchWayback(id).catch((e) => onerror(String(e)))}
		>{t('Search the Wayback Machine')}</Button
	>
	·
	<Button variant="link" onclick={addCopy} disabled={asking}>{t('Add the archived copy')}</Button>
	{#if archived !== null}
		<span class="said">
			{archived
				? t('Added the copy archived on {date}.', { date: day(archived) })
				: t('The Wayback Machine has no copy of this address.')}
		</span>
	{/if}
</dd>

<style>
	dt {
		color: var(--muted);
		font-family: var(--font-mono);
		font-size: var(--text-caption);
		letter-spacing: 0.06em;
		line-height: 19px;
		text-transform: uppercase;
	}
	dd {
		margin: 0;
		overflow-wrap: anywhere;
	}
	.said {
		display: block;
	}
</style>
