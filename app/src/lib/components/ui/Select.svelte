<script lang="ts">
	// A drop-down list; its `<option>`s are the children. Variants: `field` (full width) and
	// `inline` (sized to its content, e.g. in a toolbar).
	import type { Snippet } from 'svelte';

	let {
		value = $bindable(),
		variant = 'field',
		id,
		label,
		disabled = false,
		onchange,
		children
	}: {
		value?: string | number;
		variant?: 'field' | 'inline';
		id?: string;
		label?: string;
		disabled?: boolean;
		onchange?: (e: Event & { currentTarget: HTMLSelectElement }) => void;
		children: Snippet;
	} = $props();
</script>

<select bind:value class={variant} {id} aria-label={label} {disabled} {onchange}>
	{@render children()}
</select>

<style>
	select {
		box-sizing: border-box;
		min-height: var(--space-8);
		padding: 0 var(--space-2);
		border: 1px solid var(--line);
		border-radius: var(--radius-1);
		background: var(--bg);
		color: var(--fg);
		font: inherit;
		font-size: var(--text-body);
	}
	.field {
		width: 100%;
	}
	select:disabled {
		opacity: 0.55;
	}
</style>
