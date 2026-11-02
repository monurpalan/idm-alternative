<script lang="ts">
	// What happened when a download finished: one line per action (moved, unpacked, the
	// user's command, the webhook), red when it went wrong.

	import type { AfterDone } from '../api.ts';
	import { t } from '../i18n.svelte.ts';

	let { log }: { log: AfterDone[] } = $props();

	function text(d: AfterDone): string {
		const vars = { detail: d.detail, n: d.count ?? 0, m: d.left_out };
		if (d.action === 'move') return d.ok ? t('Moved to {detail}', vars) : t('Not moved: {detail}', vars);
		if (d.action === 'unpack') {
			if (d.count === null) return t('Not unpacked: {detail}', vars);
			return d.left_out
				? t('{n} files unpacked into {detail}; {m} left out (see the log)', vars)
				: t('{n} files unpacked into {detail}', vars);
		}
		if (d.action === 'command')
			return d.count === null ? t('Command not run: {detail}', vars) : t('{detail} ended with exit code {n}', vars);
		return d.count === null ? t('Webhook failed: {detail}', vars) : t('Told {detail} (HTTP {n})', vars);
	}
</script>

{#each log as d, i (i)}
	<dt>{i === 0 ? t('When done') : ''}</dt>
	<dd class:error={!d.ok} title={d.detail}>{text(d)}</dd>
{/each}

<style>
	dt {
		color: var(--muted);
		font-family: var(--font-mono);
		font-size: var(--text-caption);
		letter-spacing: 0.06em;
		line-height: 19px;
		text-transform: uppercase;
	}
	/* Read whole: the folder is the point of the line. */
	dd {
		margin: 0;
		overflow-wrap: anywhere;
	}
	.error {
		color: var(--danger);
	}
</style>
