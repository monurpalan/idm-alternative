<script lang="ts">
	// Several lines of text, e.g. one address per line.
	let {
		value = $bindable(''),
		rows = 3,
		placeholder,
		disabled = false,
		onblur,
		onpaste
	}: {
		value?: string;
		rows?: number;
		placeholder?: string;
		disabled?: boolean;
		onblur?: (e: FocusEvent) => void;
		onpaste?: (e: ClipboardEvent) => void;
	} = $props();
</script>

<textarea bind:value {rows} {placeholder} {disabled} {onblur} {onpaste}></textarea>

<style>
	textarea {
		box-sizing: border-box;
		width: 100%;
		padding: var(--space-2);
		border: 1px solid var(--line);
		border-radius: var(--radius-1);
		background: var(--field);
		color: var(--fg);
		font: inherit;
		font-size: var(--text-body);
		resize: vertical;
	}
	textarea::placeholder {
		color: var(--muted);
		opacity: 0.7;
	}
</style>
