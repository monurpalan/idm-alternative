<script lang="ts">
	// All downloads finished and an action was chosen for that: count down so the user can
	// stop it, like IDM's "the computer will shut down" window.

	import { onMount } from 'svelte';
	import { endCountdown, type WhenDone } from '../api.ts';
	import { msg, t } from '../i18n.svelte.ts';
	import { Button, Dialog } from './ui/index.ts';

	let {
		action,
		seconds,
		onclose
	}: { action: WhenDone; seconds: number; onclose: () => void } = $props();

	const WHAT: Record<WhenDone, string> = {
		nothing: '',
		exit: msg('idm-alternative closes in {n} seconds.'),
		sleep: msg('The computer goes to sleep in {n} seconds.'),
		hibernate: msg('The computer hibernates in {n} seconds.'),
		shutdown: msg('The computer shuts down in {n} seconds.')
	};

	let left = $state(0);
	let dialog: { close(): void } | undefined = $state();

	onMount(() => {
		left = seconds;
		const timer = setInterval(() => {
			left = Math.max(0, left - 1);
			if (left === 0) dialog?.close();
		}, 1000);
		return () => clearInterval(timer);
	});

	function cancel() {
		endCountdown().catch(() => {});
		dialog?.close();
	}

	function now() {
		endCountdown(action).catch(() => {});
		dialog?.close();
	}
</script>

<!-- Escape cancels too: closing the window without a choice must not shut the computer down. -->
<Dialog
	bind:this={dialog}
	size="narrow"
	title={t('All downloads have finished')}
	{onclose}
	oncancel={cancel}
>
	<p>{t(WHAT[action], { n: left })}</p>
	{#snippet actions()}
		<Button onclick={now}>{t('Now')}</Button>
		<Button variant="primary" onclick={cancel}>{t('Cancel')}</Button>
	{/snippet}
</Dialog>

<style>
	p {
		font-variant-numeric: tabular-nums;
	}
</style>
