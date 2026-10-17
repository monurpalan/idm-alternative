<script lang="ts">
	import { downloads, inView, VIEWS, type View } from '../downloads.svelte.ts';
	import { t } from '../i18n.svelte.ts';

	let { view = $bindable() }: { view: View } = $props();

	const counts = $derived(
		Object.fromEntries(
			VIEWS.map((v) => [v.id, downloads.tasks.filter((t) => inView(t, v.id)).length])
		) as Record<View, number>
	);
</script>

<nav aria-label={t('Categories')}>
	{#each VIEWS as v (v.id)}
		<button
			class:active={view === v.id}
			class:sub={v.id !== 'all' && v.id !== 'unfinished' && v.id !== 'finished'}
			onclick={() => (view = v.id)}
		>
			<span>{t(v.label)}</span>
			<span class="count">{counts[v.id] || ''}</span>
		</button>
	{/each}
</nav>

<style>
	nav {
		display: flex;
		flex-direction: column;
		padding: 8px 6px;
		gap: 1px;
		border-right: 1px solid var(--line);
		background: var(--panel);
		overflow-y: auto;
	}
	button {
		display: flex;
		justify-content: space-between;
		gap: 8px;
		border: 0;
		border-radius: 5px;
		background: transparent;
		color: inherit;
		text-align: left;
		padding: 6px 10px;
		cursor: pointer;
		font: inherit;
	}
	button.sub {
		padding-left: 22px;
	}
	button:hover {
		background: var(--hover);
	}
	button.active {
		background: var(--selected);
		color: var(--selected-fg);
	}
	.count {
		color: var(--muted);
		font-variant-numeric: tabular-nums;
	}
	button.active .count {
		color: inherit;
	}
</style>
