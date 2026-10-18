<script lang="ts">
	// The app's only modal window: a title, the body, and a row of actions at the bottom. It
	// opens when mounted; `close()` (or Escape) closes it and calls `onclose`. With
	// `onsubmit` the body is a form, so Enter submits it. `tall` gives it a fixed height
	// whose body grows, for a long list.
	import { onMount, type Snippet } from 'svelte';

	let {
		title,
		size = 'normal',
		tall = false,
		onclose,
		oncancel,
		onsubmit,
		children,
		actions
	}: {
		title: string;
		size?: 'narrow' | 'normal' | 'wide';
		tall?: boolean;
		onclose: () => void;
		/** Escape: when given, the dialog stays open and this decides. */
		oncancel?: () => void;
		onsubmit?: () => void;
		children: Snippet;
		actions?: Snippet;
	} = $props();

	let dialog: HTMLDialogElement | undefined = $state();
	const titleId = `dialog-${Math.random().toString(36).slice(2)}`;

	onMount(() => dialog?.showModal());

	export function close() {
		dialog?.close();
	}
</script>

{#snippet content()}
	<h1 id={titleId}>{title}</h1>
	<div class="body">{@render children()}</div>
	{#if actions}<div class="actions">{@render actions()}</div>{/if}
{/snippet}

<dialog
	bind:this={dialog}
	class={size}
	class:tall
	{onclose}
	oncancel={(e) => {
		if (!oncancel) return;
		e.preventDefault();
		oncancel();
	}}
	aria-labelledby={titleId}
>
	{#if onsubmit}
		<form
			class="frame"
			onsubmit={(e) => {
				e.preventDefault();
				onsubmit();
			}}
		>
			{@render content()}
		</form>
	{:else}
		<div class="frame">{@render content()}</div>
	{/if}
</dialog>

<style>
	dialog {
		box-sizing: border-box;
		max-height: calc(100vh - var(--space-8));
		padding: var(--space-4) var(--space-6);
		border: 1px solid var(--line);
		border-radius: var(--radius-2);
		background: var(--bg);
		color: var(--fg);
		box-shadow: var(--shadow);
	}
	dialog::backdrop {
		background: var(--backdrop);
	}
	.narrow {
		width: min(400px, calc(100vw - var(--space-8)));
	}
	.normal {
		width: min(560px, calc(100vw - var(--space-8)));
	}
	.wide {
		width: min(720px, calc(100vw - var(--space-8)));
	}
	.tall {
		height: min(560px, calc(100vh - var(--space-8)));
	}
	.frame {
		display: flex;
		flex-direction: column;
		gap: var(--space-4);
		height: 100%;
	}
	h1 {
		overflow: hidden;
		font-size: var(--text-h1);
		font-weight: 600;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.body {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		min-width: 0;
	}
	.tall .body {
		flex: 1;
		min-height: 0;
	}
	.actions {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: var(--space-2);
	}
</style>
