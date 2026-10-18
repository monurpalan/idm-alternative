<script lang="ts">
	// Text inside a window. Variants: `body`, `muted` (secondary facts), `caption` (small
	// print), `label` (the small capitals over a group or a figure) and `error` (red).
	// `mono` for figures, `truncate` keeps it to one line, `inline` for a run inside a line.
	import type { Snippet } from 'svelte';

	let {
		variant = 'body',
		mono = false,
		truncate = false,
		inline = false,
		title,
		children
	}: {
		variant?: 'body' | 'muted' | 'caption' | 'label' | 'error';
		mono?: boolean;
		truncate?: boolean;
		inline?: boolean;
		title?: string;
		children: Snippet;
	} = $props();
</script>

<svelte:element this={inline ? 'span' : 'p'} class={variant} class:mono class:truncate {title}
	>{@render children()}</svelte:element
>

<style>
	p,
	span {
		min-width: 0;
		overflow-wrap: anywhere;
		font-size: var(--text-body);
	}
	.mono {
		font-family: var(--font-mono);
		font-variant-numeric: tabular-nums;
	}
	.muted {
		color: var(--muted);
	}
	.caption {
		color: var(--muted);
		font-size: var(--text-caption);
	}
	.label {
		color: var(--muted);
		font-family: var(--font-mono);
		font-size: var(--text-caption);
		font-weight: 500;
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}
	.error {
		color: var(--danger);
	}
	.truncate {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
</style>
