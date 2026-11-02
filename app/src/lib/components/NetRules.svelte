<script lang="ts">
	// The settings' network and power rules: what waits on a metered network, the speed on
	// battery, a proxy per named network, the monthly data budget; and what the system says
	// now, with this month's data.

	import { onMount } from 'svelte';
	import { Button, Checkbox, Field, Input, Text } from './ui/index.ts';
	import { netStatus, type NetRules, type NetStatus } from '../api.ts';
	import { bytes } from '../format.ts';
	import { t } from '../i18n.svelte.ts';

	let { rules = $bindable() }: { rules: NetRules } = $props();

	let status = $state<NetStatus | null>(null);
	onMount(() => {
		netStatus().then((s) => (status = s), () => {});
	});

	// The numbers as typed: KB/s and GB, empty for none.
	let batteryKb = $state(rules.battery_limit ? String(Math.round(rules.battery_limit / 1024)) : '');
	let budgetGb = $state(rules.monthly_budget ? String(+(rules.monthly_budget / 2 ** 30).toFixed(2)) : '');
	$effect(() => {
		const kb = Number(batteryKb.trim());
		rules.battery_limit = batteryKb.trim() && kb > 0 ? Math.round(kb * 1024) : null;
	});
	$effect(() => {
		const gb = Number(budgetGb.trim().replace(',', '.'));
		rules.monthly_budget = budgetGb.trim() && gb > 0 ? Math.round(gb * 2 ** 30) : null;
	});

	const now = $derived.by(() => {
		if (!status) return '';
		const c = status.conditions;
		return [
			c.network ? t('network “{name}”', { name: c.network }) : t('an unnamed network'),
			c.metered ? t('metered') : t('not metered'),
			c.on_battery ? t('on battery') : t('on mains power')
		].join(' · ');
	});
</script>

{#if status}
	<Text variant="muted">
		{t('Now: {now}', { now })}<br />
		{status.budget
			? t('This month: {used} of {budget}', { used: bytes(status.usage.bytes), budget: bytes(status.budget) })
			: t('This month: {used}', { used: bytes(status.usage.bytes) })}
	</Text>
{/if}
<Checkbox bind:checked={rules.metered_hold}>{t('On a metered network, videos and torrents wait')}</Checkbox>
<div class="grid">
	<Field label={t('On battery, speed limit for all (KB/s)')}>
		<Input inputmode="numeric" bind:value={batteryKb} placeholder={t('none')} />
	</Field>
	<Field label={t('Monthly data budget (GB)')}>
		<Input inputmode="numeric" bind:value={budgetGb} placeholder={t('none')} />
	</Field>
</div>
<Checkbox bind:checked={rules.budget_stop}
	>{t('When the budget is used up, downloads wait until next month (otherwise it is only said)')}</Checkbox
>
<Text variant="label">{t('A proxy on a network')}</Text>
{#each rules.proxies as p, i (i)}
	<div class="row">
		<Input bind:value={p.network} placeholder={t('network name')} label={t('Network')} />
		<Input bind:value={p.proxy} placeholder="socks5://host:port" label={t('Proxy')} />
		<Button variant="ghost" onclick={() => rules.proxies.splice(i, 1)}>{t('Remove')}</Button>
	</div>
{/each}
<span>
	<Button onclick={() => rules.proxies.push({ network: status?.conditions.network ?? '', proxy: '' })}
		>{t('Add a network')}</Button
	>
</span>

<style>
	.grid {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: var(--space-3);
	}
	.row {
		display: grid;
		grid-template-columns: 1fr 1fr auto;
		gap: var(--space-2);
		align-items: center;
	}
</style>
