<script lang="ts">
	// "Only a part": start and end of a clip of a video, as m:ss or h:mm:ss, checked against
	// the video's length when it is known.

	import type { Clip } from '../api.ts';
	import { parseTime } from '../format.ts';
	import { t } from '../i18n.svelte.ts';
	import { Checkbox, Field, Input, Text } from './ui/index.ts';

	let {
		length,
		clip = $bindable(null),
		problem = $bindable(null)
	}: {
		/** The video's length in seconds, when known. */
		length: number | null;
		/** The clip asked for, or null for the whole video. */
		clip?: Clip | null;
		/** Why the times cannot be used, while they cannot. */
		problem?: string | null;
	} = $props();

	let on = $state(false);
	let start = $state('0:00');
	let end = $state('');
	let exact = $state(false);

	$effect(() => {
		if (!on) {
			clip = null;
			problem = null;
			return;
		}
		const a = parseTime(start);
		const b = parseTime(end);
		if (a === null || b === null) {
			clip = null;
			problem = t('Write the times as m:ss or h:mm:ss');
		} else if (b <= a) {
			clip = null;
			problem = t('The end must come after the start');
		} else if (length !== null && b > length + 0.5) {
			clip = null;
			problem = t('The video is only {time} long', { time: clock(length) });
		} else {
			clip = { start: a, end: b, exact };
			problem = null;
		}
	});

	/** `1:05` or `1:02:03`. */
	function clock(seconds: number): string {
		const s = Math.max(0, Math.floor(seconds));
		const two = (n: number) => String(n).padStart(2, '0');
		return s >= 3600
			? `${Math.floor(s / 3600)}:${two(Math.floor(s / 60) % 60)}:${two(s % 60)}`
			: `${Math.floor(s / 60)}:${two(s % 60)}`;
	}

	function toggle() {
		if (on && !end && length !== null) end = clock(length);
	}
</script>

<Checkbox bind:checked={on} onchange={toggle}>{t('Only a part')}</Checkbox>
{#if on}
	<div class="times">
		<Field label={t('From')}>
			<Input bind:value={start} placeholder="0:00" />
		</Field>
		<Field label={t('To')}>
			<Input bind:value={end} placeholder={length !== null ? clock(length) : '1:00'} />
		</Field>
		<Checkbox bind:checked={exact}>{t('Cut at the exact frame (slower: re-encodes)')}</Checkbox>
	</div>
	{#if problem}<Text variant="error">{problem}</Text>{:else}<Text variant="caption"
			>{t('Only the needed part is downloaded; the file name gets the range.')}</Text
		>{/if}
{/if}

<style>
	.times {
		display: grid;
		grid-template-columns: 96px 96px 1fr;
		align-items: end;
		gap: var(--space-2);
	}
</style>
