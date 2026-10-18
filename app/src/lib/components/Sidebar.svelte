<script lang="ts">
	import { downloads, inView, VIEWS, type View } from '../downloads.svelte.ts';
	import { t } from '../i18n.svelte.ts';
	import { Button } from './ui/index.ts';

	let { view = $bindable() }: { view: View } = $props();

	const counts = $derived(
		Object.fromEntries(
			VIEWS.map((v) => [v.id, downloads.tasks.filter((t) => inView(t, v.id)).length])
		) as Record<View, number>
	);
</script>

<nav aria-label={t('Categories')}>
	{#each VIEWS as v (v.id)}
		<div class:sub={v.id !== 'all' && v.id !== 'unfinished' && v.id !== 'finished'}>
			<Button variant="nav" active={view === v.id} onclick={() => (view = v.id)}>
				<span>{t(v.label)}</span>
				<span class="count">{counts[v.id] || ''}</span>
			</Button>
		</div>
	{/each}
</nav>

<style>
	nav {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		padding: var(--space-2);
		border-right: 1px solid var(--line);
		background: var(--panel);
		overflow-y: auto;
	}
	.sub {
		padding-left: var(--space-3);
	}
	.count {
		color: var(--muted);
		font-size: var(--text-caption);
		font-variant-numeric: tabular-nums;
	}
</style>
