<script lang="ts">
	// Asked before a queue starts when a drive has not enough room for it: move those downloads
	// to another drive, start anyway (they pause before the drive fills), or not start.

	import { open as pickFolder } from '@tauri-apps/plugin-dialog';
	import type { DriveNeed } from '../api.ts';
	import { bytes } from '../format.ts';
	import { t } from '../i18n.svelte.ts';
	import { Button, Dialog, Text } from './ui/index.ts';

	let {
		needs,
		onmove,
		onstart,
		onclose
	}: {
		/** The drives that are short. */
		needs: DriveNeed[];
		/** Moves the downloads on those drives into `dir`, then starts them. */
		onmove: (dir: string) => void;
		onstart: () => void;
		onclose: () => void;
	} = $props();

	let dialog: { close(): void } | undefined = $state();

	async function move() {
		const dir = await pickFolder({ directory: true });
		if (typeof dir !== 'string') return;
		onmove(dir);
		dialog?.close();
	}
</script>

<Dialog bind:this={dialog} size="narrow" title={t('Not enough disk space')} {onclose} onsubmit={move}>
	{#each needs as n (n.volume)}
		<p>
			<Text variant="error" inline
				>{t('{missing} short on {volume}', { missing: bytes(n.short ?? 0), volume: n.volume })}</Text
			>
			<br />
			<Text variant="muted" inline>
				{t('{need} to download, {free} free, {reserve} kept free', {
					need: bytes(n.need),
					free: bytes(n.free ?? 0),
					reserve: bytes(n.reserve)
				})}
				{#if n.unknown}
					· {t('and {n} of unknown size', { n: n.unknown })}
				{/if}
			</Text>
		</p>
	{/each}
	<Text variant="muted"
		>{t('Started anyway, downloads pause by themselves before the drive fills up.')}</Text
	>
	{#snippet actions()}
		<Button onclick={() => dialog?.close()}>{t('Cancel')}</Button>
		<Button
			onclick={() => {
				onstart();
				dialog?.close();
			}}>{t('Start anyway')}</Button
		>
		<Button type="submit" variant="primary">{t('Move to another drive…')}</Button>
	{/snippet}
</Dialog>
