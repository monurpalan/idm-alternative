<script lang="ts">
	// The app's only button. Variants:
	// - primary: the main action of a window (accent);
	// - secondary: every other action (default);
	// - ghost: quiet actions inside lists and panels;
	// - link: an action that reads as a link;
	// - nav: an entry of a side list, `active` when chosen;
	// - column: a table column's heading, which sorts by it (`align` to its numbers);
	// - menu: an entry of a context menu (`danger` for one that deletes).
	import type { Snippet } from 'svelte';
	import Icon, { type IconName } from './Icon.svelte';

	type Variant = 'primary' | 'secondary' | 'ghost' | 'link' | 'nav' | 'column' | 'menu';

	let {
		variant = 'secondary',
		type = 'button',
		icon,
		label,
		title,
		disabled = false,
		active = false,
		pressed,
		align = 'start',
		danger = false,
		role,
		onclick,
		children
	}: {
		variant?: Variant;
		type?: 'button' | 'submit';
		/** An icon before the text, or alone (then give `label`). */
		icon?: IconName;
		/** The accessible name, for a button without text. */
		label?: string;
		title?: string;
		disabled?: boolean;
		/** `nav`: the chosen entry. */
		active?: boolean;
		/** A toggle's state (`aria-pressed`). */
		pressed?: boolean;
		/** `column`: which side the heading sits on. */
		align?: 'start' | 'end';
		/** `menu`: an action that deletes, in the accent. */
		danger?: boolean;
		role?: 'menuitem';
		onclick?: (e: MouseEvent) => void;
		children?: Snippet;
	} = $props();
</script>

<button
	{type}
	class={variant}
	class:active
	class:icon-only={!children}
	class:end={align === 'end'}
	class:danger
	{role}
	{disabled}
	{title}
	aria-label={label}
	aria-pressed={pressed}
	{onclick}
>
	{#if icon}<Icon name={icon} />{/if}
	{#if children}{@render children()}{/if}
</button>

<style>
	button {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: var(--space-2);
		min-height: var(--space-8);
		padding: 0 var(--space-3);
		border: 1px solid var(--line);
		border-radius: var(--radius-1);
		background: var(--field);
		color: var(--fg);
		font: inherit;
		font-size: var(--text-body);
		white-space: nowrap;
		cursor: pointer;
	}
	button:hover:not(:disabled) {
		background: var(--hover);
	}
	button:disabled {
		opacity: 0.45;
		cursor: default;
	}
	.icon-only {
		padding: 0 var(--space-2);
	}

	.primary {
		border-color: var(--accent);
		background: var(--accent);
		color: var(--on-accent);
		font-weight: 600;
	}
	.primary:hover:not(:disabled) {
		background: var(--accent);
		filter: brightness(1.08);
	}

	.secondary[aria-pressed='true'] {
		border-color: var(--accent);
		background: var(--selected);
	}

	.ghost {
		border-color: transparent;
		background: transparent;
	}

	.link {
		min-height: 0;
		padding: 0;
		border: 0;
		background: transparent;
		color: var(--accent);
		text-decoration: underline;
	}
	.link:hover:not(:disabled) {
		background: transparent;
	}

	.nav {
		justify-content: space-between;
		width: 100%;
		border-color: transparent;
		background: transparent;
		text-align: left;
	}
	.nav.active {
		background: var(--selected);
	}

	.column {
		justify-content: flex-start;
		gap: var(--space-1);
		width: 100%;
		min-height: var(--space-8);
		padding: 0 var(--space-2);
		border: 0;
		border-radius: 0;
		background: transparent;
		color: var(--muted);
		font-weight: 500;
	}
	.column:hover:not(:disabled) {
		background: transparent;
		color: var(--fg);
	}
	.column.end {
		justify-content: flex-end;
	}

	.menu {
		justify-content: flex-start;
		width: 100%;
		border-color: transparent;
		background: transparent;
		text-align: left;
	}
	.menu:disabled {
		color: var(--muted);
		opacity: 1;
	}
	.danger {
		color: var(--accent);
	}
</style>
