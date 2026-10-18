<script lang="ts">
	// The side column, in inverted colors: the app's mark, the way to add downloads, the
	// lists, and a readout of what is moving now with the one-time "when all finish" action.
	import type { WhenDone } from '../api.ts';
	import { downloads, inView, VIEWS, type View } from '../downloads.svelte.ts';
	import { speed } from '../format.ts';
	import { msg, t } from '../i18n.svelte.ts';
	import { Button, Select, Text } from './ui/index.ts';

	let {
		view = $bindable(),
		limit,
		whenDone,
		onadd,
		ongrab,
		onsettings,
		onwhendone
	}: {
		view: View;
		/** The speed limit for all downloads, bytes/s. */
		limit: number | null;
		whenDone: WhenDone;
		onadd: () => void;
		ongrab: () => void;
		onsettings: () => void;
		onwhendone: (action: WhenDone) => void;
	} = $props();

	const STATES: View[] = ['all', 'unfinished', 'finished'];
	const groups = [
		{ label: msg('Status'), views: VIEWS.filter((v) => STATES.includes(v.id)) },
		{ label: msg('Type'), views: VIEWS.filter((v) => !STATES.includes(v.id)) }
	];

	const counts = $derived(
		Object.fromEntries(
			VIEWS.map((v) => [v.id, downloads.tasks.filter((t) => inView(t, v.id)).length])
		) as Record<View, number>
	);
</script>

<aside class="invert">
	<div class="brand">
		<span class="mark" aria-hidden="true"><i></i><i></i><i></i><i></i></span>
		<span class="name">idm-alternative</span>
		<Button variant="ghost" icon="settings" onclick={onsettings} label={t('Settings')} title={t('Settings')} />
	</div>

	<div class="add">
		<Button variant="primary" icon="plus" align="start" onclick={onadd} title="Ctrl+N">{t('New download')}</Button>
		<Button variant="ghost" icon="globe" align="start" onclick={ongrab}>{t('Grab files from a site')}</Button>
	</div>

	<nav aria-label={t('Lists')}>
		{#each groups as g (g.label)}
			<div class="group">
				<span class="heading"><Text variant="label" inline>{t(g.label)}</Text></span>
				{#each g.views as v (v.id)}
					<Button variant="nav" active={view === v.id} onclick={() => (view = v.id)}>
						<span>{t(v.label)}</span>
						<span class="count">{counts[v.id] || ''}</span>
					</Button>
				{/each}
			</div>
		{/each}
	</nav>

	<section class="meter" aria-label={t('Now')}>
		<Text variant="label">{t('Now')}</Text>
		<p class="speed">{speed(downloads.speed) || t('{size}/s', { size: '0 B' })}</p>
		<Text variant="caption" mono>
			{t('{n} running', { n: downloads.active })}{limit
				? ` · ${t('limit {speed}', { speed: speed(limit) })}`
				: ''}
		</Text>
		<label class="when">
			<Text variant="label" inline>{t('When all finish')}</Text>
			<Select value={whenDone} onchange={(e) => onwhendone(e.currentTarget.value as WhenDone)}>
				<option value="nothing">{t('Do nothing')}</option>
				<option value="exit">{t('Close the app')}</option>
				<option value="sleep">{t('Sleep')}</option>
				<option value="hibernate">{t('Hibernate')}</option>
				<option value="shutdown">{t('Shut down')}</option>
			</Select>
		</label>
	</section>
</aside>

<style>
	aside {
		border-right: 1px solid var(--line);
		display: flex;
		flex-direction: column;
		min-height: 0;
		overflow-y: auto;
	}
	.brand {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		height: var(--space-14);
		padding: 0 var(--space-2) 0 var(--space-4);
		flex: none;
	}
	.mark {
		display: flex;
		gap: 2px;
	}
	.mark i {
		width: 6px;
		height: var(--space-3);
		background: var(--accent);
	}
	.mark i:last-child {
		background: var(--faint);
	}
	.name {
		flex: 1;
		font-family: var(--font-mono);
		font-size: var(--text-body);
		font-weight: 500;
		letter-spacing: -0.01em;
	}
	.add {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		padding: 0 var(--space-3) var(--space-4);
		flex: none;
	}
	nav {
		display: flex;
		flex-direction: column;
		gap: var(--space-4);
		flex: 1;
	}
	.group {
		display: flex;
		flex-direction: column;
	}
	.heading {
		padding: 0 var(--space-4) var(--space-1);
	}
	.count {
		color: var(--faint);
		font-family: var(--font-mono);
		font-size: var(--text-caption);
		font-variant-numeric: tabular-nums;
	}
	.meter {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		margin-top: var(--space-4);
		padding: var(--space-4);
		border-top: 1px solid var(--line);
		flex: none;
	}
	.speed {
		color: var(--fg);
		font-family: var(--font-mono);
		font-size: var(--text-h1);
		font-weight: 500;
		font-variant-numeric: tabular-nums;
	}
	.when {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		margin-top: var(--space-3);
	}
</style>
