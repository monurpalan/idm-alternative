<script lang="ts">
	// A new name for a download; its files on disk follow. Opens with the name before the
	// extension selected, as Explorer does.

	import { onMount } from 'svelte';
	import { renameTask, type Task } from '../api.ts';
	import { t } from '../i18n.svelte.ts';
	import { Button, Dialog, Input, Text } from './ui/index.ts';

	let { task, onclose }: { task: Task; onclose: () => void } = $props();

	// Taken once: the dialog edits its own copy of the name.
	const original = (() => task.name ?? '')();
	let name = $state(original);
	let error = $state<string | null>(null);
	let busy = $state(false);
	let dialog: { close(): void } | undefined = $state();
	let body: HTMLDivElement | undefined = $state();

	onMount(() => {
		const input = body?.querySelector('input');
		const dot = original.lastIndexOf('.');
		input?.setSelectionRange(0, dot > 0 ? dot : original.length);
	});

	async function submit() {
		if (!name.trim() || name === original) {
			dialog?.close();
			return;
		}
		busy = true;
		try {
			await renameTask(task.id, name);
			dialog?.close();
		} catch (e) {
			error = String(e);
		} finally {
			busy = false;
		}
	}
</script>

<Dialog bind:this={dialog} size="narrow" title={t('Rename')} {onclose} onsubmit={submit}>
	<div class="body" bind:this={body}>
		<Input bind:value={name} label={t('New name')} required />
		{#if error}<Text variant="error">{error}</Text>{/if}
	</div>
	{#snippet actions()}
		<Button onclick={() => dialog?.close()}>{t('Cancel')}</Button>
		<Button type="submit" variant="primary" disabled={busy}>{t('Rename')}</Button>
	{/snippet}
</Dialog>

<style>
	.body {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}
</style>
