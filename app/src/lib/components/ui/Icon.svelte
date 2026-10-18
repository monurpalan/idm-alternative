<script lang="ts" module>
	/** Every icon the app draws, as 24×24 stroke paths. */
	const PATHS = {
		settings: [
			'M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z',
			'M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1Z'
		],
		close: ['M6 6l12 12', 'M18 6L6 18'],
		plus: ['M12 5v14', 'M5 12h14'],
		play: ['M7 5l12 7-12 7Z'],
		pause: ['M8 5v14', 'M16 5v14'],
		trash: ['M4 7h16', 'M9 7V4h6v3', 'M6 7l1 13h10l1-13'],
		folder: ['M3 6h6l2 2h10v11H3Z'],
		globe: ['M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18Z', 'M3 12h18', 'M12 3c3 3 3 15 0 18', 'M12 3c-3 3-3 15 0 18'],
		search: ['M11 18a7 7 0 1 0 0-14 7 7 0 0 0 0 14Z', 'M20 20l-4-4'],
		open: ['M14 4h6v6', 'M20 4l-9 9', 'M18 14v6H4V6h6'],
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
		stroke-width: 1.75;
		stroke-linecap: square;
		stroke-linejoin: miter;
	}
</style>
