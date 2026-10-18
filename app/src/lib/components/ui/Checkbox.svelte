<script lang="ts">
	// A checkbox with its text, or a radio button of a group when `group` is bound.
	import type { Snippet } from 'svelte';

	let {
		checked = $bindable(false),
		group = $bindable(),
		value,
		name,
		type = 'checkbox',
		disabled = false,
		onchange,
		children
	}: {
		checked?: boolean;
		/** `radio`: the group's chosen value. */
		group?: unknown;
		/** `radio`: this button's value. */
		value?: unknown;
		name?: string;
		type?: 'checkbox' | 'radio';
		disabled?: boolean;
		onchange?: (e: Event) => void;
		children?: Snippet;
	} = $props();
</script>

<label class:disabled>
	{#if type === 'radio'}
		<input type="radio" {name} {value} bind:group {disabled} {onchange} />
	{:else}
		<input type="checkbox" bind:checked {disabled} {onchange} />
	{/if}
	{#if children}<span>{@render children()}</span>{/if}
</label>

<style>
	label {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		min-width: 0;
		color: var(--fg);
		font-size: var(--text-body);
		cursor: pointer;
	}
	span {
		min-width: 0;
	}
	input {
		margin: 0;
		accent-color: var(--accent);
	}
	.disabled {
		opacity: 0.55;
		cursor: default;
	}
</style>
