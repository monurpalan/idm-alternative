<script lang="ts">
	// What a download holds, read from its skeleton while it downloads: an archive's files, a
	// video's length and picture size, a document's pages. Seen early, a wrong file can be
	// stopped before the rest of it comes.

	import type { Skeleton } from '../api.ts';
	import { bytes, duration } from '../format.ts';
	import { t } from '../i18n.svelte.ts';

	let { skeleton }: { skeleton: Skeleton } = $props();

	const text = $derived.by(() => {
		switch (skeleton.kind) {
			case 'archive':
				return t('{n} files, {size} unpacked', { n: skeleton.files, size: bytes(skeleton.unpacked) });
			case 'video':
				return skeleton.width
					? t('{length} · {width}×{height}', {
							length: duration(skeleton.duration),
							width: skeleton.width,
							height: skeleton.height
						})
					: duration(skeleton.duration);
			case 'document':
				return t('{n} pages', { n: skeleton.pages });
		}
	});
	const names = $derived(skeleton.kind === 'archive' ? skeleton.names : []);
	const more = $derived(skeleton.kind === 'archive' ? skeleton.files - skeleton.names.length : 0);
</script>

<dt>{t('Contents')}</dt>
<dd>
	{text}
	{#if names.length}
		<span class="names" title={names.join('\n')}
			>{names.slice(0, 5).join(', ')}{names.length > 5 || more > 0
				? ` ${t('and {n} more', { n: skeleton.kind === 'archive' ? skeleton.files - Math.min(5, names.length) : 0 })}`
				: ''}</span
		>
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
	.names {
		display: block;
		color: var(--muted);
		font-size: var(--text-caption);
	}
</style>
