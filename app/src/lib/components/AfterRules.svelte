<script lang="ts">
	// The settings' rules for finished downloads: which ones (type, site, name) and what
	// happens to them (unpack, move, the user's command, a webhook). Only these rules can run
	// a program or call an address; a download from the browser or an MCP client cannot.

	import { open as pickFolder } from '@tauri-apps/plugin-dialog';
	import { Button, Checkbox, Field, Input, Select, Text } from './ui/index.ts';
	import type { Rule } from '../api.ts';
	import { msg, t } from '../i18n.svelte.ts';

	let { rules = $bindable() }: { rules: Rule[] } = $props();

	const CATEGORIES = [
		{ id: '', label: msg('Any type') },
		{ id: 'compressed', label: msg('Compressed') },
		{ id: 'documents', label: msg('Documents') },
		{ id: 'music', label: msg('Music') },
		{ id: 'programs', label: msg('Programs') },
		{ id: 'video', label: msg('Video') },
		{ id: 'general', label: msg('General') }
	] as const;

	function add() {
		rules.push({
			category: null,
			site: null,
			name: null,
			unpack: false,
			move_to: null,
			command: null,
			webhook: null
		});
	}

	async function browse(r: Rule) {
		const picked = await pickFolder({ directory: true, defaultPath: r.move_to ?? undefined });
		if (typeof picked === 'string') r.move_to = picked;
	}
</script>

<Text variant="muted">
	{t('When a download finishes, every rule it fits applies. A rule with no condition fits every download.')}
</Text>
{#each rules as r, i (i)}
	<section class="rule" aria-label={t('Rule {n}', { n: i + 1 })}>
		<div class="when">
			<Field label={t('Type')}>
				<Select
					value={r.category ?? ''}
					onchange={(e) => (r.category = (e.currentTarget.value || null) as Rule['category'])}
				>
					{#each CATEGORIES as c (c.id)}<option value={c.id}>{t(c.label)}</option>{/each}
				</Select>
			</Field>
			<Field label={t('Site')}>
				<Input bind:value={r.site} placeholder="example.com" />
			</Field>
			<Field label={t('File name')}>
				<Input bind:value={r.name} placeholder="*.iso" />
			</Field>
		</div>
		<Checkbox bind:checked={r.unpack}>{t('Unpack archives into a folder next to them (ZIP, tar)')}</Checkbox>
		<Field label={t('Move to')} layout="row">
			<Input bind:value={r.move_to} placeholder={t('leave where it is')} />
			<Button onclick={() => browse(r)}>{t('Browse…')}</Button>
		</Field>
		<Field label={t('Run a program')}>
			<Input bind:value={r.command} placeholder={'"C:\\Tools\\tool.exe" --in "{file}"'} />
		</Field>
		<Field label={t('Tell a web address (webhook)')}>
			<Input bind:value={r.webhook} placeholder="https://…" />
		</Field>
		<div class="foot">
			<Text variant="caption" inline>
				{t('The program runs without a shell; {file} and {dir} are the file and its folder.')}
			</Text>
			<Button variant="ghost" onclick={() => rules.splice(i, 1)}>{t('Remove rule')}</Button>
		</div>
	</section>
{/each}
<span>
	<Button onclick={add}>{t('Add a rule')}</Button>
</span>

<style>
	.rule {
		display: grid;
		gap: var(--space-2);
		padding: var(--space-3);
		border: 1px solid var(--line);
	}
	.when {
		display: grid;
		grid-template-columns: repeat(3, minmax(0, 1fr));
		gap: var(--space-2);
	}
	.foot {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-2);
	}
</style>
