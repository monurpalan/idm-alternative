<script lang="ts">
	import { onMount } from 'svelte';
	import { open as pickFolder } from '@tauri-apps/plugin-dialog';
	import { getSettings, setSettings, type Settings } from '../api.ts';

	let { onclose }: { onclose: () => void } = $props();

	let s = $state<Settings | null>(null);
	/** The limit as typed, in KB/s; empty for none. */
	let limitKb = $state('');
	let error = $state<string | null>(null);
	let saving = $state(false);
	let dialog: HTMLDialogElement | undefined = $state();

	onMount(() => {
		dialog?.showModal();
		getSettings()
			.then((v) => {
				s = v;
				limitKb = v.speed_limit ? String(Math.round(v.speed_limit / 1024)) : '';
			})
			.catch((e) => (error = String(e)));
	});

	async function browse() {
		if (!s) return;
		const picked = await pickFolder({ directory: true, defaultPath: s.dir || undefined });
		if (typeof picked === 'string') s.dir = picked;
	}

	async function save(e: SubmitEvent) {
		e.preventDefault();
		if (!s) return;
		const kb = Number(limitKb.trim());
		if (limitKb.trim() && !(kb > 0)) {
			error = 'The speed limit must be a positive number of KB/s, or empty.';
			return;
		}
		saving = true;
		try {
			await setSettings({
				...s,
				max_active: Number(s.max_active),
				connections: Number(s.connections),
				speed_limit: limitKb.trim() ? Math.round(kb * 1024) : null
			});
			dialog?.close();
		} catch (err) {
			error = String(err);
		} finally {
			saving = false;
		}
	}
</script>

<dialog bind:this={dialog} onclose={onclose} aria-labelledby="settings-title">
	<form onsubmit={save}>
		<h2 id="settings-title">Settings</h2>
		{#if s}
			<fieldset>
				<legend>Downloads</legend>
				<label class="wide">
					Save to
					<div class="row">
						<input bind:value={s.dir} />
						<button type="button" onclick={browse}>Browse…</button>
					</div>
				</label>
				<label class="check">
					<input type="checkbox" bind:checked={s.per_category} />
					Sort into folders by type (Video, Music, Programs…)
				</label>
				<label class="check">
					<input type="checkbox" bind:checked={s.overwrite} />
					Replace files with the same name (otherwise save as “name (1)”)
				</label>
				<div class="grid">
					<label>
						Downloads at once
						<input type="number" min="1" max="50" bind:value={s.max_active} />
					</label>
					<label>
						Connections per download
						<input type="number" min="1" max="64" bind:value={s.connections} />
					</label>
					<label>
						Speed limit, all downloads (KB/s)
						<input inputmode="numeric" bind:value={limitKb} placeholder="none" />
					</label>
				</div>
			</fieldset>

			<fieldset>
				<legend>Connection</legend>
				<div class="grid">
					<label>
						Look like
						<select bind:value={s.browser}>
							<option value="chrome">Chrome</option>
							<option value="edge">Edge</option>
							<option value="firefox">Firefox</option>
							<option value="safari">Safari</option>
							<option value="none">Plain client</option>
						</select>
					</label>
					<label class="span2">
						Proxy
						<input bind:value={s.proxy} placeholder="none, or socks5://host:port" />
					</label>
				</div>
			</fieldset>

			<fieldset>
				<legend>App</legend>
				<label class="check">
					<input type="checkbox" bind:checked={s.close_to_tray} />
					Keep running in the tray when the window is closed
				</label>
				<label class="check">
					<input type="checkbox" bind:checked={s.start_with_windows} />
					Start with Windows (in the tray)
				</label>
				<label class="check">
					<input type="checkbox" bind:checked={s.notify} />
					Notify me when a download finishes
				</label>
				<label class="check">
					<input type="checkbox" bind:checked={s.browser_integration} />
					Take downloads from the browser extension (registers with Chrome, Edge, Brave,
					Vivaldi, Opera and Firefox)
				</label>
			</fieldset>
		{:else if !error}
			<p class="muted">Loading…</p>
		{/if}

		{#if error}<p class="error">{error}</p>{/if}

		<div class="buttons">
			<button type="button" onclick={() => dialog?.close()}>Cancel</button>
			<button type="submit" class="primary" disabled={!s || saving}>Save</button>
		</div>
	</form>
</dialog>

<style>
	dialog {
		width: min(620px, calc(100vw - 32px));
		max-height: calc(100vh - 48px);
		border: 1px solid var(--line);
		border-radius: 10px;
		background: var(--bg);
		color: var(--fg);
		padding: 18px 20px;
		box-shadow: 0 12px 40px rgb(0 0 0 / 0.25);
	}
	dialog::backdrop {
		background: rgb(0 0 0 / 0.3);
	}
	h2 {
		font-size: 16px;
		margin: 0 0 12px;
	}
	fieldset {
		border: 1px solid var(--line);
		border-radius: 8px;
		margin: 0 0 12px;
		padding: 8px 12px 10px;
	}
	legend {
		color: var(--muted);
		padding: 0 4px;
	}
	label {
		display: flex;
		flex-direction: column;
		gap: 4px;
		color: var(--muted);
	}
	label.wide {
		margin-bottom: 8px;
	}
	label.check {
		flex-direction: row;
		align-items: center;
		gap: 8px;
		color: var(--fg);
		margin: 6px 0;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(3, 1fr);
		gap: 10px;
		margin-top: 8px;
	}
	.span2 {
		grid-column: span 2;
	}
	input:not([type='checkbox']),
	select {
		padding: 5px 8px;
		border: 1px solid var(--line);
		border-radius: 5px;
		background: var(--field);
		color: var(--fg);
		font: inherit;
		min-width: 0;
	}
	.row {
		display: flex;
		gap: 6px;
	}
	.row input {
		flex: 1;
	}
	.muted {
		color: var(--muted);
	}
	.error {
		color: var(--danger);
	}
	.buttons {
		display: flex;
		justify-content: flex-end;
		gap: 8px;
		margin-top: 4px;
	}
</style>
