<script lang="ts" module>
	export interface Item {
		/** The English text, marked with msg where it is written; shown translated. */
		label: string;
		action: () => void;
		disabled?: boolean;
		danger?: boolean;
		/** Draw a separator above this item. */
		separated?: boolean;
	}
</script>

<script lang="ts">
	// A right-click menu at the pointer; closes on any outside click, Escape, or after a choice.

	import { t } from '../i18n.svelte.ts';
	import { Button, placeAt } from './ui/index.ts';

	let {
		x,
		y,
		items,
		onclose
	}: { x: number; y: number; items: Item[]; onclose: () => void } = $props();

	let menu: HTMLDivElement | undefined = $state();

	$effect(() => menu?.focus());

	function choose(item: Item) {
		if (item.disabled) return;
		onclose();
		item.action();
	}
</script>

<svelte:window
	onpointerdown={(e) => {
		if (menu && !menu.contains(e.target as Node)) onclose();
	}}
	onkeydown={(e) => {
		if (e.key === 'Escape') onclose();
	}}
	onblur={onclose}
/>

<div class="menu" role="menu" tabindex="-1" bind:this={menu} use:placeAt={{ x, y }}>
	{#each items as item (item.label)}
		{#if item.separated}<hr />{/if}
		<Button
			variant="menu"
			role="menuitem"
			disabled={item.disabled}
			danger={item.danger}
			onclick={() => choose(item)}>{t(item.label)}</Button
		>
	{/each}
</div>

<style>
	.menu {
		position: fixed;
		z-index: 20;
		display: flex;
		flex-direction: column;
		min-width: 192px;
		padding: var(--space-1);
		border: 1px solid var(--line);
		border-radius: var(--radius-2);
		background: var(--bg);
		box-shadow: var(--shadow);
		outline: none;
	}
	hr {
		width: 100%;
		margin: var(--space-1) 0;
		border: 0;
		border-top: 1px solid var(--line);
	}
</style>
