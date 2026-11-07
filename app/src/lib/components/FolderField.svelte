<script lang="ts">
	// Where a download goes: typed, or picked with "Browse…". Empty means the default folder
	// (per type, when the settings say so), shown as the placeholder.

	import { open as pickFolder } from '@tauri-apps/plugin-dialog';
	import { onMount } from 'svelte';
	import { getSettings } from '../api.ts';
	import { t } from '../i18n.svelte.ts';
	import { Button, Field, Input } from './ui/index.ts';

	let { dir = $bindable('') }: { dir: string } = $props();
	let defaultDir = $state('');

	onMount(() => {
		getSettings()
			.then((s) => (defaultDir = s.per_category ? `${s.dir}\\<type>` : s.dir))
			.catch(() => {});
	});

	async function browse() {
		const picked = await pickFolder({ directory: true, defaultPath: dir || undefined });
		if (typeof picked === 'string') dir = picked;
	}
</script>

<Field label={t('Folder')} layout="row">
	<Input bind:value={dir} placeholder={defaultDir} />
	<Button onclick={browse}>{t('Browse…')}</Button>
</Field>
