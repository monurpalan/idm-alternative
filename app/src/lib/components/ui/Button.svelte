<script lang="ts">
	// The app's only button. Variants:
	// - primary: the main action of a window (accent; red with `danger`, for deleting);
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
		align,
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
		/** Which side the content sits on (centered by default). */
		align?: 'start' | 'end';
		/** `menu`: an action that deletes, in red. */
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
	class:start={align === 'start'}
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
		background: transparent;
		color: var(--fg);
		font: inherit;
		font-size: var(--text-body);
		font-weight: 500;
		white-space: nowrap;
		cursor: pointer;
	}
	button:hover:not(:disabled) {
		border-color: var(--line-strong);
		background: var(--hover);
	}
	button:disabled {
		opacity: 0.4;
		cursor: default;
	}
	.icon-only {
		width: var(--space-8);
		padding: 0;
	}

	.primary {
		border-color: var(--accent);
		background: var(--accent);
		color: var(--on-accent);
		font-weight: 600;
	}
	.primary:hover:not(:disabled) {
		border-color: var(--accent);
		background: color-mix(in srgb, var(--accent) 88%, var(--paper));
	}

	.primary.danger {
		border-color: var(--danger);
		background: var(--danger);
		color: var(--paper);
	}
	.primary.danger:hover:not(:disabled) {
		border-color: var(--danger);
		background: color-mix(in srgb, var(--danger) 88%, var(--paper));
	}

	.secondary[aria-pressed='true'] {
		border-color: var(--accent);
		background: var(--selected);
	}

	.ghost {
		border-color: transparent;
	}
	.ghost:hover:not(:disabled) {
		border-color: transparent;
	}

	.link {
		min-height: 0;
		padding: 0;
		border: 0;
		color: var(--accent);
		text-decoration: underline;
		text-underline-offset: 3px;
	}
	.link:hover:not(:disabled) {
		background: transparent;
	}

	/* A side-list entry: a bar of the accent marks the chosen one. */
	.nav {
		justify-content: space-between;
		width: 100%;
		padding: 0 var(--space-4);
		border: 0;
		border-radius: 0;
		color: var(--muted);
		font-weight: 400;
		text-align: left;
	}
	.nav:hover:not(:disabled) {
		color: var(--fg);
	}
	.nav.active {
		background: var(--hover);
		box-shadow: inset 2px 0 0 var(--accent);
		color: var(--fg);
		font-weight: 500;
	}

	.column {
		justify-content: flex-start;
		gap: var(--space-1);
		width: 100%;
		padding: 0 var(--space-2);
		border: 0;
		border-radius: 0;
		color: var(--muted);
		font-family: var(--font-mono);
		font-size: var(--text-caption);
		font-weight: 500;
		letter-spacing: 0.06em;
		text-transform: uppercase;
	}
	.column:hover:not(:disabled) {
		background: transparent;
		color: var(--fg);
	}
	.start {
		justify-content: flex-start;
	}
	.end {
		justify-content: flex-end;
	}

	.menu {
		justify-content: flex-start;
		width: 100%;
		border: 0;
		border-radius: 0;
		font-weight: 400;
		text-align: left;
	}
	.menu:disabled {
		color: var(--faint);
		opacity: 1;
	}
	.danger {
		color: var(--danger);
	}
</style>
