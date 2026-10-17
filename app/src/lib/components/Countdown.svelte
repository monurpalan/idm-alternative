<script lang="ts">
	// All downloads finished and an action was chosen for that: count down so the user can
	// stop it, like IDM's "the computer will shut down" window.

	import { onMount } from 'svelte';
	import { endCountdown, type WhenDone } from '../api.ts';
	import { msg, t } from '../i18n.svelte.ts';

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
	let dialog: HTMLDialogElement | undefined = $state();

	onMount(() => {
		left = seconds;
		dialog?.showModal();
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
<dialog
	bind:this={dialog}
	onclose={onclose}
	oncancel={(e) => {
		e.preventDefault();
		cancel();
	}}
	aria-labelledby="countdown-title"
>
	<h2 id="countdown-title">{t('All downloads have finished')}</h2>
	<p>{t(WHAT[action], { n: left })}</p>
	<div class="buttons">
		<button type="button" onclick={now}>{t('Now')}</button>
		<button type="button" class="primary" onclick={cancel}>{t('Cancel')}</button>
	</div>
</dialog>

<style>
	dialog {
		width: min(380px, calc(100vw - 32px));
		border: 1px solid var(--line);
		border-radius: 10px;
		background: var(--bg);
		color: var(--fg);
		padding: 18px 20px;
		box-shadow: 0 12px 40px rgb(0 0 0 / 0.25);
	}
	dialog::backdrop {
		background: rgb(0 0 0 / 0.3);
	}
	h2 {
		font-size: 15px;
		font-weight: 600;
		margin: 0 0 10px;
	}
	p {
		margin: 0;
		font-variant-numeric: tabular-nums;
	}
	.buttons {
		display: flex;
		justify-content: flex-end;
		gap: 8px;
		margin-top: 18px;
	}
</style>
