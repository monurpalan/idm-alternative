<script lang="ts">
	// A download's deadline in its details: by when, from where (its own or its link's), how
	// it stands, and setting or clearing it.

	import { Button, Input } from './ui/index.ts';
	import { deadlines, setDeadline, type DeadlineView, type Task } from '../api.ts';
	import { moment, speed } from '../format.ts';
	import { t } from '../i18n.svelte.ts';

	let { task, onerror }: { task: Task; onerror: (message: string | null) => void } = $props();

	let view = $state<DeadlineView | null>(null);
	let editing = $state(false);
	let value = $state('');

	// Looked at again while the download runs: its standing changes with its speed.
	$effect(() => {
		const id = task.id;
		void task.deadline;
		void task.status;
		let stop = false;
		const look = () =>
			deadlines()
				.then((all) => {
					if (!stop && id === task.id) view = all.find((v) => v.id === id) ?? null;
				})
				.catch(() => {});
		look();
		const timer = setInterval(look, 2000);
		return () => {
			stop = true;
			clearInterval(timer);
		};
	});

	/** `YYYY-MM-DDTHH:MM` in local time, as the input takes it. */
	function local(ms: number): string {
		const d = new Date(ms - new Date(ms).getTimezoneOffset() * 60_000);
		return d.toISOString().slice(0, 16);
	}

	function edit() {
		value = local(view?.at ?? Date.now() + 3600_000);
		editing = true;
	}

	async function save(at: number | null) {
		try {
			await setDeadline(task.id, at);
			editing = false;
			onerror(null);
		} catch (e) {
			onerror(String(e));
		}
	}

	const standing = $derived(
		!view
			? ''
			: view.standing === 'late'
				? t('late')
				: view.standing === 'at_risk'
					? t('at risk: needs {speed}', { speed: speed(view.needed) })
					: t('on time')
	);
</script>

{#if task.status !== 'done'}
	<dt>{t('Deadline')}</dt>
	<dd class:error={view?.standing === 'at_risk' || view?.standing === 'late'}>
		{#if editing}
			<span class="edit">
				<Input type="datetime-local" bind:value label={t('Deadline')} />
				<Button variant="link" onclick={() => save(value ? new Date(value).getTime() : null)}>{t('Set')}</Button>
				<Button variant="link" onclick={() => (editing = false)}>{t('Cancel')}</Button>
			</span>
		{:else if view}
			{view.source === 'link'
				? t('by {time}, when its link is expected to stop working', { time: moment(view.at) })
				: t('by {time}', { time: moment(view.at) })}
			· {standing}
			<Button variant="link" onclick={edit}>{t('Change…')}</Button>
			{#if view.source === 'own'}
				<Button variant="link" onclick={() => save(null)}>{t('Clear')}</Button>
			{/if}
		{:else}
			{t('none')}
			<Button variant="link" onclick={edit}>{t('Set…')}</Button>
		{/if}
	</dd>
{/if}

<style>
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
		overflow-wrap: anywhere;
	}
	.edit {
		display: flex;
		align-items: center;
		gap: var(--space-2);
	}
	.error {
		color: var(--danger);
	}
</style>
