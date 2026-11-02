<script lang="ts">
	// "Why slow?" in a running download's details: one line per thing that holds it back,
	// only when known for certain, with the fix where there is one. Nothing for a download
	// that has none.

	import { Button } from './ui/index.ts';
	import { getSettings, setSettings, setSpeedLimit, type Hint, type TaskId } from '../api.ts';
	import { speed } from '../format.ts';
	import { t } from '../i18n.svelte.ts';

	let { id, hints, onerror }: { id: TaskId; hints: Hint[]; onerror: (message: string | null) => void } = $props();

	function text(h: Hint): string {
		switch (h.kind) {
			case 'limit':
				return h.own
					? t('Held at {speed} by its own speed limit', { speed: speed(h.rate) })
					: t('Held at {speed} by the speed limit for all downloads', { speed: speed(h.rate) });
			case 'disk':
				return t('The disk cannot write as fast as the data arrives');
			case 'drops':
				return t('Connections keep dropping: {n} reopened in the last minute', { n: h.count });
			case 'server':
				return t('The server allows only {n} connections at a time', { n: h.connections });
			case 'proxy':
				return t('Going through the proxy set in the settings');
		}
	}

	async function removeLimit(own: boolean) {
		try {
			if (own) await setSpeedLimit(id, null);
			else await setSettings({ ...(await getSettings()), speed_limit: null });
			onerror(null);
		} catch (err) {
			onerror(String(err));
		}
	}
</script>

{#each hints as h, i (h.kind)}
	<dt>{i === 0 ? t('Why slow') : ''}</dt>
	<dd>
		{text(h)}
		{#if h.kind === 'limit'}
			<Button variant="link" onclick={() => removeLimit(h.own)}>{t('Remove the limit')}</Button>
		{/if}
	</dd>
{/each}

<style>
	dt {
		color: var(--muted);
		font-family: var(--font-mono);
		font-size: var(--text-caption);
		letter-spacing: 0.06em;
		line-height: 19px;
		text-transform: uppercase;
	}
	/* Read whole, with its button, rather than cut off. */
	dd {
		margin: 0;
	}
</style>
