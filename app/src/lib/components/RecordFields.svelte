<script lang="ts">
	// How a live stream is recorded: from the start of the broadcast (sites that keep it),
	// for how long, and when to begin.

	import type { Recording } from '../api.ts';
	import { t } from '../i18n.svelte.ts';
	import { Checkbox, Field, Input, Text } from './ui/index.ts';

	let {
		fromStartOffered = false,
		record = $bindable({ limit: null, from_start: false, start_at: null }),
		problem = $bindable(null)
	}: {
		/** The site keeps the broadcast from its start (yt-dlp's `--live-from-start`). */
		fromStartOffered?: boolean;
		record?: Recording;
		problem?: string | null;
	} = $props();

	let fromStart = $state(false);
	let limited = $state(false);
	let minutes = $state('60');
	let later = $state(false);
	let at = $state('');

	$effect(() => {
		const m = Number(minutes);
		problem = null;
		if (limited && !(Number.isFinite(m) && m > 0)) problem = t('Give the minutes to record');
		let startAt: number | null = null;
		if (later) {
			const match = /^(\d{1,2}):(\d{2})$/.exec(at.trim());
			if (!match || Number(match[1]) > 23 || Number(match[2]) > 59) {
				problem = t('Give the start time as HH:MM');
			} else {
				// Today at that time, or tomorrow when it has passed.
				const d = new Date();
				d.setHours(Number(match[1]), Number(match[2]), 0, 0);
				if (d.getTime() <= Date.now()) d.setDate(d.getDate() + 1);
				startAt = d.getTime();
			}
		}
		record = {
			limit: limited && m > 0 ? Math.round(m * 60) : null,
			from_start: fromStartOffered && fromStart,
			start_at: startAt
		};
	});
</script>

<Text variant="muted">{t('A live broadcast: it is recorded until it ends or you stop it (Pause).')}</Text>
{#if fromStartOffered}
	<Checkbox bind:checked={fromStart}>{t('From the start of the broadcast, not from now')}</Checkbox>
{/if}
<div class="row">
	<Checkbox bind:checked={limited}>{t('Stop after')}</Checkbox>
	<Field label={t('Minutes')}>
		<Input inputmode="numeric" bind:value={minutes} disabled={!limited} />
	</Field>
</div>
<div class="row">
	<Checkbox bind:checked={later}>{t('Begin at')}</Checkbox>
	<Field label={t('Time (HH:MM)')}>
		<Input bind:value={at} placeholder="21:00" disabled={!later} />
	</Field>
</div>
{#if problem}<Text variant="error">{problem}</Text>{/if}

<style>
	.row {
		display: grid;
		grid-template-columns: 160px 120px;
		align-items: end;
		gap: var(--space-2);
	}
</style>
