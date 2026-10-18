<script lang="ts">
	// A progress bar cut into small segments, after the engine that splits a download into
	// ranges, with its figure beside it. `value` 0–1, or null when the size is unknown.
	// States: `running` (accent), `done` (faint green), `stopped` (faint), `error` (faint red).
	let {
		value,
		state = 'running',
		text = '',
		label
	}: {
		value: number | null;
		state?: 'running' | 'done' | 'stopped' | 'error';
		text?: string;
		label?: string;
	} = $props();
</script>

<span class="bar {state}">
	<progress max="1" value={value ?? 0} aria-label={label}></progress>
	<span class="text">{text}</span>
</span>

<style>
	.bar {
		display: grid;
		grid-template-columns: 1fr var(--space-14);
		align-items: center;
		gap: var(--space-2);
		min-width: 0;
	}
	progress {
		display: block;
		width: 100%;
		height: var(--space-2);
		border: 0;
		border-radius: 0;
		appearance: none;
		background: var(--track);
		/* 6px segments with 2px gaps. */
		mask-image: repeating-linear-gradient(90deg, var(--ink) 0 6px, transparent 6px 8px);
	}
	progress::-webkit-progress-bar {
		background: var(--track);
	}
	progress::-webkit-progress-value {
		background: var(--accent);
	}
	progress::-moz-progress-bar {
		background: var(--accent);
	}
	.done progress::-webkit-progress-value {
		background: color-mix(in srgb, var(--success) 45%, transparent);
	}
	.done progress::-moz-progress-bar {
		background: color-mix(in srgb, var(--success) 45%, transparent);
	}
	.stopped progress::-webkit-progress-value {
		background: var(--faint);
	}
	.stopped progress::-moz-progress-bar {
		background: var(--faint);
	}
	.error progress::-webkit-progress-value {
		background: color-mix(in srgb, var(--danger) 45%, transparent);
	}
	.error progress::-moz-progress-bar {
		background: color-mix(in srgb, var(--danger) 45%, transparent);
	}
	.text {
		overflow: hidden;
		color: var(--muted);
		font-family: var(--font-mono);
		font-size: var(--text-caption);
		font-variant-numeric: tabular-nums;
		text-align: right;
		white-space: nowrap;
	}
	.running .text {
		color: var(--fg);
	}
</style>
