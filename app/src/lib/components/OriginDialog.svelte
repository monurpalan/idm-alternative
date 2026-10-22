<script lang="ts">
	// "Where did this file come from?": the download that saved it (or the mark the browser
	// left on it), with its checksum checked again, a fresh download, and a look for a newer
	// version.

	import { open as pickFile } from '@tauri-apps/plugin-dialog';
	import {
		addTask,
		checkFileChecksum,
		checkForUpdate,
		fileOrigin,
		type ChecksumCheck,
		type Origin,
		type UpdateCheck
	} from '../api.ts';
	import { bytes, day } from '../format.ts';
	import { t } from '../i18n.svelte.ts';
	import { Button, Dialog, Text } from './ui/index.ts';

	let {
		origin: given = null,
		dropped = null,
		onclose
	}: {
		/** What is known already; none: the user chooses the file here. */
		origin?: Origin | null;
		/** The name of a dropped file that the history does not know. */
		dropped?: string | null;
		onclose: () => void;
	} = $props();

	let origin = $state<Origin | null>(null);
	let error = $state<string | null>(null);
	let busy = $state(false);
	let checksum = $state<ChecksumCheck | null>(null);
	let update = $state<UpdateCheck | null>(null);
	let added = $state(false);
	let dialog: { close(): void } | undefined = $state();

	$effect.pre(() => {
		origin = given;
	});

	const how = $derived(
		!origin
			? ''
			: origin.traced === 'path'
				? t('Downloaded here, by Shardload.')
				: origin.traced === 'content'
					? t('The same file as one Shardload downloaded (same size and content).')
					: origin.traced === 'mark'
						? t('Not downloaded by Shardload; the browser noted where it came from.')
						: t('Nothing says where this file came from.')
	);

	async function run<T>(p: Promise<T>): Promise<T | undefined> {
		busy = true;
		error = null;
		try {
			return await p;
		} catch (e) {
			error = String(e);
		} finally {
			busy = false;
		}
	}

	async function choose() {
		const path = await pickFile({ directory: false });
		if (typeof path !== 'string') return;
		checksum = null;
		update = null;
		added = false;
		const o = await run(fileOrigin(path));
		if (o) origin = o;
	}

	async function check() {
		if (!origin) return;
		const c = await run(checkFileChecksum(origin.path, origin.checksum, origin.url));
		if (c) checksum = c;
	}

	async function newer() {
		if (!origin?.url) return;
		const u = await run(checkForUpdate(origin.url, origin.size, origin.downloaded_at));
		if (u) update = u;
	}

	async function again() {
		if (!origin?.url) return;
		const headers: [string, string][] = origin.page ? [['Referer', origin.page]] : [];
		const done = await run(
			addTask({ url: origin.url, name: null, dir: null, start: true, login: null, headers })
		);
		if (done) added = true;
	}
</script>

<Dialog bind:this={dialog} title={t('Where did this file come from?')} {onclose}>
	{#if origin}
		<Text truncate title={origin.path}>{origin.path}</Text>
		<Text variant="muted">{bytes(origin.size)} · {how}</Text>
		<dl>
			{#if origin.url}
				<dt>{t('Address')}</dt>
				<dd title={origin.url}>{origin.url}</dd>
			{/if}
			{#if origin.page}
				<dt>{t('Page')}</dt>
				<dd title={origin.page}>{origin.page}</dd>
			{/if}
			{#if origin.downloaded_at}
				<dt>{t('Downloaded')}</dt>
				<dd>{day(origin.downloaded_at)}</dd>
			{/if}
			{#if origin.checksum}
				<dt>{t('Checksum')}</dt>
				<dd class="mono" title={origin.checksum}>{origin.checksum}</dd>
			{/if}
		</dl>
		{#if checksum}
			{#if checksum.matches === null}
				<Text variant="muted">{t('There is no checksum to check it against, and the publisher posts none.')}</Text>
			{:else if checksum.matches}
				<Text>{t('The file matches its checksum ({from}).', { from: checksum.from ?? '' })}</Text>
			{:else}
				<Text variant="error">{t('The file does not match its checksum ({from}): it changed or is damaged.', { from: checksum.from ?? '' })}</Text>
			{/if}
		{/if}
		{#if update}
			{#if update.latest_release && update.release && update.latest_release !== update.release}
				<Text>{t('A newer release is out: {latest} (this file is from {release}).', { latest: update.latest_release, release: update.release })}</Text>
			{:else if update.latest_release}
				<Text>{t('This is the newest release ({release}).', { release: update.latest_release })}</Text>
			{/if}
			{#if update.changed === true}
				<Text>{t('The address serves a different file now ({size}).', { size: bytes(update.size) || '?' })}</Text>
			{:else if update.changed === false}
				<Text variant="muted">{t('The address serves the same file as when it was downloaded.')}</Text>
			{:else if !update.latest_release}
				<Text variant="muted">{t('The server does not say whether it changed.')}</Text>
			{/if}
		{/if}
		{#if added}<Text>{t('Added to the list.')}</Text>{/if}
		<div>
			<Button variant="link" onclick={choose} disabled={busy}>{t('Another file…')}</Button>
		</div>
	{:else if dropped}
		<Text>{t('Shardload has not downloaded {name}.', { name: dropped })}</Text>
		<Text variant="muted">{t('Choose the file: Windows keeps the address it was downloaded from with it, and so does Linux.')}</Text>
	{:else}
		<Text variant="muted">{t('Choose a file to see where it came from.')}</Text>
	{/if}
	{#if error}<Text variant="error">{error}</Text>{/if}

	{#snippet actions()}
		{#if origin}
			<Button onclick={check} disabled={busy}>{t('Check the checksum')}</Button>
			<Button onclick={newer} disabled={busy || !origin.url}>{t('Newer version?')}</Button>
			<Button variant="primary" onclick={again} disabled={busy || !origin.url || added}
				>{t('Download again')}</Button
			>
		{:else}
			<Button onclick={() => dialog?.close()}>{t('Close')}</Button>
			<Button variant="primary" onclick={choose} disabled={busy}>{t('Choose a file…')}</Button>
		{/if}
	{/snippet}
</Dialog>

<style>
	dl {
		display: grid;
		grid-template-columns: 96px 1fr;
		gap: var(--space-1) var(--space-2);
		margin: 0;
	}
	dt {
		color: var(--muted);
		font-family: var(--font-mono);
		font-size: var(--text-caption);
		letter-spacing: 0.06em;
		line-height: 19px;
		text-transform: uppercase;
	}
	dd {
		margin: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.mono {
		font-family: var(--font-mono);
		font-size: var(--text-caption);
	}
</style>
