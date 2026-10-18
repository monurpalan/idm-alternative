<script lang="ts">
	// A progress bar with its text on top. `value` 0–1, or null when the size is unknown.
	// Variants by state: `running` and `error` (accent), `done` (the text color), `stopped`
	// (faint). Fills are soft so the text on top stays readable.
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
	{#if text}<span class="text">{text}</span>{/if}
</span>

<style>
	.bar {
		position: relative;
		display: block;
		height: var(--space-4);
	}
	progress {
		display: block;
		width: 100%;
		height: 100%;
		border: 0;
		border-radius: var(--radius-1);
		appearance: none;
		overflow: hidden;
		background: var(--track);
	}
	progress::-webkit-progress-bar {
		background: var(--track);
	}
	progress::-webkit-progress-value {
		background: color-mix(in srgb, var(--accent) 50%, transparent);
	}
	progress::-moz-progress-bar {
		background: color-mix(in srgb, var(--accent) 50%, transparent);
	}
	.done progress::-webkit-progress-value {
		background: color-mix(in srgb, var(--fg) 26%, transparent);
	}
	.done progress::-moz-progress-bar {
		background: color-mix(in srgb, var(--fg) 26%, transparent);
	}
	.stopped progress::-webkit-progress-value {
		background: color-mix(in srgb, var(--fg) 14%, transparent);
	}
	.stopped progress::-moz-progress-bar {
		background: color-mix(in srgb, var(--fg) 14%, transparent);
	}
	.error progress::-webkit-progress-value {
		background: color-mix(in srgb, var(--accent) 30%, transparent);
	}
	.error progress::-moz-progress-bar {
		background: color-mix(in srgb, var(--accent) 30%, transparent);
	}
	.text {
		position: absolute;
		inset: 0;
		display: flex;
		align-items: center;
		justify-content: center;
		color: var(--fg);
		font-size: var(--text-caption);
		font-variant-numeric: tabular-nums;
	}
</style>
