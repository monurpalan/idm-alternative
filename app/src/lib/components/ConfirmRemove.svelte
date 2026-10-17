<script lang="ts">
	// Asks before taking downloads off the list, optionally with their files.

	import { onMount } from 'svelte';
	import { t } from '../i18n.svelte.ts';

	let {
		count,
		onconfirm,
		onclose
	}: { count: number; onconfirm: (deleteFiles: boolean) => void; onclose: () => void } =
		$props();

	let deleteFiles = $state(false);
	let dialog: HTMLDialogElement | undefined = $state();

	onMount(() => dialog?.showModal());
</script>

<dialog bind:this={dialog} onclose={onclose} aria-labelledby="remove-title">
	<form
		method="dialog"
		onsubmit={() => {
			onconfirm(deleteFiles);
		}}
	>
		<h2 id="remove-title">
			{count === 1
				? t('Remove this download from the list?')
				: t('Remove {n} downloads from the list?', { n: count })}
		</h2>
		<label class="check">
			<input type="checkbox" bind:checked={deleteFiles} />
			{count === 1 ? t('Also delete the file from disk') : t('Also delete the files from disk')}
		</label>
		<div class="buttons">
			<button type="button" onclick={() => dialog?.close()}>{t('Cancel')}</button>
			<button type="submit" class:danger={deleteFiles} class="primary">{t('Remove')}</button>
		</div>
	</form>
</dialog>

<style>
	dialog {
		width: min(420px, calc(100vw - 32px));
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
		margin: 0 0 14px;
	}
	.check {
		display: flex;
		align-items: center;
		gap: 8px;
	}
	.buttons {
		display: flex;
		justify-content: flex-end;
		gap: 8px;
		margin-top: 18px;
	}
	.primary.danger {
		background: var(--danger);
		border-color: var(--danger);
	}
</style>
