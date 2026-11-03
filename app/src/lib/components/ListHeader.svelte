<script lang="ts">
	// The bar above the list: the view's name and what all of it can do, or what the selected
	// downloads can; and the search.
	import type { Task } from '../api.ts';
	import { t } from '../i18n.svelte.ts';
	import { canPause, canStart } from '../taskMenu.ts';
	import { Button, Input, Text } from './ui/index.ts';

	let {
		title,
		count,
		selected,
		anyToStart,
		anyToPause,
		search = $bindable(),
		onresume,
		onpause,
		onremove,
		onclear,
		onresumeall,
		onpauseall
	}: {
		/** The view's name, in English. */
		title: string;
		/** Downloads on the list. */
		count: number;
		selected: Task[];
		anyToStart: boolean;
		anyToPause: boolean;
		search: string;
		onresume: () => void;
		onpause: () => void;
		onremove: () => void;
		onclear: () => void;
		onresumeall: () => void;
		onpauseall: () => void;
	} = $props();
</script>

<header>
	{#if selected.length}
		<Text variant="label">{t('{n} selected', { n: selected.length })}</Text>
		<div class="actions">
			<Button icon="play" onclick={onresume} disabled={!selected.some(canStart)}>{t('Resume')}</Button>
			<Button icon="pause" onclick={onpause} disabled={!selected.some(canPause)}>{t('Pause')}</Button>
			<Button icon="trash" onclick={onremove} title="Delete">{t('Remove')}</Button>
			<Button variant="ghost" icon="close" onclick={onclear} label={t('Clear the selection')} title="Esc" />
		</div>
	{:else}
		<h1>{t(title)}</h1>
		<Text variant="caption" mono inline>{count}</Text>
		<div class="actions">
			{#if anyToStart}
				<Button variant="ghost" icon="play" onclick={onresumeall}>{t('Resume all')}</Button>
			{/if}
			{#if anyToPause}
				<Button variant="ghost" icon="pause" onclick={onpauseall}>{t('Pause all')}</Button>
			{/if}
		</div>
	{/if}
	<span class="search">
		<Input variant="search" type="search" bind:value={search} placeholder={t('Search')} label={t('Search')} />
	</span>
</header>

<style>
	header {
		display: flex;
		align-items: center;
		gap: var(--space-3);
		height: var(--space-14);
		padding: 0 var(--space-4);
		border-bottom: 1px solid var(--line-strong);
		white-space: nowrap;
	}
	h1 {
		font-size: var(--text-h1);
		font-weight: 600;
		letter-spacing: -0.01em;
	}
	.actions {
		display: flex;
		align-items: center;
		gap: var(--space-2);
	}
	.search {
		display: flex;
		width: 224px;
		margin-left: auto;
	}
</style>
