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

	let {
		x,
		y,
		items,
		onclose
	}: { x: number; y: number; items: Item[]; onclose: () => void } = $props();

	let menu: HTMLDivElement | undefined = $state();
	let pos = $state({ left: 0, top: 0 });

	// Keep the menu inside the window.
	$effect(() => {
		if (!menu) return;
		const r = menu.getBoundingClientRect();
		pos = {
			left: Math.min(x, window.innerWidth - r.width - 4),
			top: Math.min(y, window.innerHeight - r.height - 4)
		};
		menu.focus();
	});

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

<div
	class="menu"
	role="menu"
	tabindex="-1"
	bind:this={menu}
	style:left="{pos.left}px"
	style:top="{pos.top}px"
>
	{#each items as item (item.label)}
		{#if item.separated}<hr />{/if}
		<button
			role="menuitem"
			disabled={item.disabled}
			class:danger={item.danger}
			onclick={() => choose(item)}>{t(item.label)}</button
		>
	{/each}
</div>

<style>
	.menu {
		position: fixed;
		z-index: 20;
		min-width: 190px;
		padding: 4px;
		border: 1px solid var(--line);
		border-radius: 7px;
		background: var(--bg);
		box-shadow: 0 6px 24px rgb(0 0 0 / 0.18);
		outline: none;
	}
	button {
		display: block;
		width: 100%;
		border: 0;
		border-radius: 4px;
		background: transparent;
		color: inherit;
		text-align: left;
		padding: 6px 10px;
		font: inherit;
		cursor: pointer;
	}
	button:hover:not(:disabled) {
		background: var(--hover);
	}
	button:disabled {
		color: var(--muted);
		cursor: default;
	}
	.danger {
		color: var(--danger);
	}
	hr {
		border: 0;
		border-top: 1px solid var(--line);
		margin: 4px 2px;
	}
</style>
