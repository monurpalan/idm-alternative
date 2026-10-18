<script lang="ts" module>
	/** Every icon the app draws, as 24×24 stroke paths. */
	const PATHS = {
		settings: [
			'M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z',
			'M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1Z'
		],
		close: ['M6 6l12 12', 'M18 6L6 18'],
		'arrow-up': ['M12 19V5', 'M6 11l6-6 6 6'],
		'arrow-down': ['M12 5v14', 'M18 13l-6 6-6-6']
	} as const;

	export type IconName = keyof typeof PATHS;
</script>

<script lang="ts">
	// One place for the app's icons; components name them instead of embedding SVG.
	let { name }: { name: IconName } = $props();
</script>

<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
	{#each PATHS[name] as d (d)}
		<path {d} />
	{/each}
</svg>

<style>
	svg {
		width: var(--space-4);
		height: var(--space-4);
		flex: none;
		fill: none;
		stroke: currentColor;
		stroke-width: 2;
		stroke-linecap: round;
		stroke-linejoin: round;
	}
</style>
