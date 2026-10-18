<script lang="ts">
	// The app's only text-like field. Variants: `field` (default, full width) and `search`
	// (a compact field in a toolbar).
	type Kind = 'text' | 'password' | 'url' | 'number' | 'time' | 'search';

	let {
		value = $bindable(),
		type = 'text',
		variant = 'field',
		id,
		placeholder,
		label,
		title,
		disabled = false,
		required = false,
		inputmode,
		autocomplete,
		min,
		max,
		onblur,
		onkeydown,
		oninput
	}: {
		value?: string | number | null;
		type?: Kind;
		variant?: 'field' | 'search';
		id?: string;
		placeholder?: string;
		/** The accessible name when no `<label>` names it. */
		label?: string;
		title?: string;
		disabled?: boolean;
		required?: boolean;
		inputmode?: 'url' | 'numeric' | 'text';
		autocomplete?: 'username' | 'current-password' | 'off';
		min?: number;
		max?: number;
		onblur?: (e: FocusEvent) => void;
		onkeydown?: (e: KeyboardEvent) => void;
		oninput?: (e: Event) => void;
	} = $props();
</script>

<input
	bind:value
	{type}
	class={variant}
	{id}
	{placeholder}
	{title}
	aria-label={label}
	{disabled}
	{required}
	{inputmode}
	{autocomplete}
	{min}
	{max}
	{onblur}
	{onkeydown}
	{oninput}
/>

<style>
	input {
		box-sizing: border-box;
		width: 100%;
		min-width: 0;
		min-height: var(--space-8);
		padding: 0 var(--space-2);
		border: 1px solid var(--line);
		border-radius: var(--radius-1);
		background: var(--field);
		color: var(--fg);
		font: inherit;
		font-size: var(--text-body);
	}
	input::placeholder {
		color: var(--muted);
		opacity: 0.7;
	}
	input:disabled {
		opacity: 0.55;
	}
	.search {
		width: auto;
		flex: 1;
	}
</style>
