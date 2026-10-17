<script lang="ts">
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
