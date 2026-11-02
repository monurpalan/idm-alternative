<script lang="ts">
	// A finished file without a checksum, checked by its own structure: sound, damaged parts
	// downloaded again, or damaged on the server too.

	import type { Structure } from '../api.ts';
	import { bytes } from '../format.ts';
	import { t } from '../i18n.svelte.ts';

	let { structure }: { structure: Structure } = $props();

	const text = $derived(
		structure.damaged
			? t('Damaged: the server sends it so too; get it from elsewhere if you can', { format: structure.format })
			: structure.repaired
				? t('{format}, checked by its own structure: {size} were damaged and downloaded again', {
						format: structure.format,
						size: bytes(structure.repaired)
					})
				: t('{format}, checked by its own structure: sound', { format: structure.format })
	);
</script>

<dt>{t('Structure')}</dt>
<dd class:error={structure.damaged}>{text}</dd>

<style>
	dt {
		color: var(--muted);
		font-family: var(--font-mono);
		font-size: var(--text-caption);
		letter-spacing: 0.06em;
		line-height: 19px;
		text-transform: uppercase;
	}
	/* Read whole. */
	dd {
		margin: 0;
		overflow-wrap: anywhere;
	}
	.error {
		color: var(--danger);
	}
</style>
