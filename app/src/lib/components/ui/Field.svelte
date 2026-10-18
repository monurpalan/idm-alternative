<script lang="ts">
	// A labelled control: the caption above, the control (and anything beside it) below.
	// `row` puts the children side by side, e.g. a field and its "Browse…" button.
	import type { Snippet } from 'svelte';

	let {
		label,
		layout = 'stack',
		children
	}: {
		label: string;
		layout?: 'stack' | 'row';
		children: Snippet;
	} = $props();
</script>

<label>
	<span class="caption">{label}</span>
	<span class="control {layout}">{@render children()}</span>
</label>

<style>
	label {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		min-width: 0;
	}
	.caption {
		color: var(--muted);
	}
	.control {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		min-width: 0;
	}
	.control.row {
		flex-direction: row;
		align-items: center;
	}
</style>
