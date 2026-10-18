<script lang="ts">
	// Asks before taking downloads off the list, optionally with their files.

	import { t } from '../i18n.svelte.ts';
	import { Button, Checkbox, Dialog } from './ui/index.ts';

	let {
		count,
		onconfirm,
		onclose
	}: { count: number; onconfirm: (deleteFiles: boolean) => void; onclose: () => void } =
		$props();

	let deleteFiles = $state(false);
	let dialog: { close(): void } | undefined = $state();
</script>

<Dialog
	bind:this={dialog}
	size="narrow"
	title={count === 1
		? t('Remove this download from the list?')
		: t('Remove {n} downloads from the list?', { n: count })}
	{onclose}
	onsubmit={() => {
		onconfirm(deleteFiles);
		dialog?.close();
	}}
>
	<Checkbox bind:checked={deleteFiles}>
		{count === 1 ? t('Also delete the file from disk') : t('Also delete the files from disk')}
	</Checkbox>
	{#snippet actions()}
		<Button onclick={() => dialog?.close()}>{t('Cancel')}</Button>
		<Button type="submit" variant="primary" danger>{t('Remove')}</Button>
	{/snippet}
</Dialog>
