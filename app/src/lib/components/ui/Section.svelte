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
		padding: var(--space-3) 0 0;
		border: 0;
		border-top: 1px solid var(--line);
	}
	legend {
		padding: 0 var(--space-2) 0 0;
		color: var(--muted);
		font-family: var(--font-mono);
		font-size: var(--text-caption);
		font-weight: 500;
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}
</style>
