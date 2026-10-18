<script lang="ts">
	// A titled group of controls inside a dialog.
	import type { Snippet } from 'svelte';

	let { title, children }: { title: string; children: Snippet } = $props();
</script>

<fieldset>
	<legend>{title}</legend>
	{@render children()}
</fieldset>

<style>
	fieldset {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		min-width: 0;
		margin: 0;
		padding: var(--space-2) var(--space-3) var(--space-3);
		border: 1px solid var(--line);
		border-radius: var(--radius-2);
	}
	legend {
		padding: 0 var(--space-1);
		color: var(--muted);
		font-size: var(--text-h2);
		font-weight: 600;
	}
</style>
