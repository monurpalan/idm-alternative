<script lang="ts">
	// The two typefaces ship with the app (Latin and Turkish letters only), no network.
	import '@fontsource/ibm-plex-sans/latin-400.css';
	import '@fontsource/ibm-plex-sans/latin-ext-400.css';
	import '@fontsource/ibm-plex-sans/latin-500.css';
	import '@fontsource/ibm-plex-sans/latin-ext-500.css';
	import '@fontsource/ibm-plex-sans/latin-600.css';
	import '@fontsource/ibm-plex-sans/latin-ext-600.css';
	import '@fontsource/ibm-plex-mono/latin-400.css';
	import '@fontsource/ibm-plex-mono/latin-ext-400.css';
	import '@fontsource/ibm-plex-mono/latin-500.css';
	import '@fontsource/ibm-plex-mono/latin-ext-500.css';
	import '../app.css';
	import { onMount } from 'svelte';
	import { onSettings, uiLanguage } from '#lib/api.ts';
	import { i18n } from '#lib/i18n.svelte.ts';

	let { children } = $props();

	// The language comes from the app (the setting, or Windows'), for every window.
	onMount(() => {
		const apply = () =>
			uiLanguage()
				.then((l) => i18n.set(l))
				.catch(() => {});
		apply();
		const unlisten = onSettings(apply);
		return () => {
			unlisten.then((f) => f());
		};
	});
</script>

{@render children()}
