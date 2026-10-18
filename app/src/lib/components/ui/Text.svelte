<script lang="ts">
	// A line of text inside a window. Variants: `body`, `muted` (secondary facts), `caption`
	// (small print) and `error` (accent). `truncate` keeps it to one line.
	import type { Snippet } from 'svelte';

	let {
		variant = 'body',
		truncate = false,
		title,
		children
	}: {
		variant?: 'body' | 'muted' | 'caption' | 'error';
		truncate?: boolean;
		title?: string;
		children: Snippet;
	} = $props();
</script>

<p class={variant} class:truncate {title}>{@render children()}</p>

<style>
	p {
		min-width: 0;
		overflow-wrap: anywhere;
		font-size: var(--text-body);
	}
	.muted {
		color: var(--muted);
	}
	.caption {
		color: var(--muted);
		font-size: var(--text-caption);
	}
	.error {
		color: var(--accent);
	}
	.truncate {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
</style>
