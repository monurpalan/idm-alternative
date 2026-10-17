<script lang="ts">
	import { onMount } from 'svelte';
	import { open as pickFolder } from '@tauri-apps/plugin-dialog';
	import {
		getSettings,
		checkUpdate,
		forgetLogin,
		savedLogins,
		type SavedLogin,
		installTool,
		onToolsUpdated,
		setSettings,
		tools,
		type Settings,
		type ToolStatus
	} from '../api.ts';

	let { onclose }: { onclose: () => void } = $props();

	const DAYS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];

	let s = $state<Settings | null>(null);
	/** The limit as typed, in KB/s; empty for none. */
	let limitKb = $state('');
	let error = $state<string | null>(null);
	let saving = $state(false);
	let dialog: HTMLDialogElement | undefined = $state();
	let toolStatus = $state<ToolStatus | null>(null);
	let installing = $state<'ytdlp' | 'deno' | null>(null);
	let toolError = $state<string | null>(null);
	let logins = $state<SavedLogin[]>([]);

	async function forget(site: string) {
		await forgetLogin(site).catch((e) => (error = String(e)));
		logins = await savedLogins().catch(() => logins);
	}

	/** The result of "Check now". */
	let updateStatus = $state<string | null>(null);

	async function checkNow() {
		updateStatus = 'Checking…';
		try {
			const u = await checkUpdate();
			updateStatus = u
				? `Version ${u.version} is available: install it from the bar at the bottom of the window.`
				: 'This is the latest version.';
		} catch (e) {
			updateStatus = `Could not check: ${e}`;
		}
	}

	async function setUp(tool: 'ytdlp' | 'deno') {
		installing = tool;
		toolError = null;
		try {
			await installTool(tool);
			toolStatus = await tools();
		} catch (e) {
			toolError = String(e);
		} finally {
			installing = null;
		}
	}

	onMount(() => {
		dialog?.showModal();
		const refresh = () =>
			tools()
				.then((t) => (toolStatus = t))
				.catch(() => {});
		refresh();
		const unlisten = onToolsUpdated(refresh);
		savedLogins()
			.then((l) => (logins = l))
			.catch(() => {});
		getSettings()
			.then((v) => {
				s = v;
				limitKb = v.speed_limit ? String(Math.round(v.speed_limit / 1024)) : '';
			})
			.catch((e) => (error = String(e)));
		return () => {
			unlisten.then((f) => f());
		};
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
				<legend>Scheduler</legend>
				<label class="check">
					<input type="checkbox" bind:checked={s.schedule.enabled} />
					Start and stop the queue at set times
				</label>
				<div class="grid">
					<label>
						Start at
						<input type="time" bind:value={s.schedule.start} disabled={!s.schedule.enabled} />
					</label>
					<label>
						Stop at
						<input type="time" bind:value={s.schedule.stop} disabled={!s.schedule.enabled} />
					</label>
				</div>
				<div class="days" role="group" aria-label="Days">
					{#each DAYS as day, i (day)}
						<label class="day">
							<input
								type="checkbox"
								bind:checked={s.schedule.days[i]}
								disabled={!s.schedule.enabled}
							/>
							{day}
						</label>
					{/each}
				</div>
				<p class="small">
					Leave a time empty to only start or only stop. "When done" in the toolbar can shut the
					computer down after the queue finishes.
				</p>
			</fieldset>

			<fieldset>
				<legend>Saved logins</legend>
				{#each logins as l (l.site)}
					<div class="login-row">
						<span>{l.site}</span>
						<span class="muted">{l.user}</span>
						<button type="button" onclick={() => forget(l.site)}>Forget</button>
					</div>
				{:else}
					<p class="muted">
						None yet. Tick “Remember for this site” when a server asks for a password.
					</p>
				{/each}
			</fieldset>

			<fieldset>
				<legend>Video tools</legend>
				{#if toolStatus}
					<div class="tool">
						<span>ffmpeg</span>
						<span class="muted">
							{toolStatus.ffmpeg ? 'Found' : 'Not found: install it (e.g. winget install ffmpeg) to join video and audio'}
						</span>
					</div>
					{#each [{ id: 'ytdlp', name: 'yt-dlp', why: 'YouTube, X, Instagram and other sites' }, { id: 'deno', name: 'deno', why: 'Needed by yt-dlp for YouTube' }] as t (t.id)}
						{@const version = toolStatus[t.id as 'ytdlp' | 'deno']}
						<div class="tool">
							<span>{t.name}</span>
							<span class="muted">{version ? `Version ${version}` : t.why}</span>
							<button
								type="button"
								disabled={installing !== null}
								onclick={() => setUp(t.id as 'ytdlp' | 'deno')}
							>
								{installing === t.id ? 'Downloading…' : version ? 'Update' : 'Set up'}
							</button>
						</div>
					{/each}
					<label class="check">
						<input type="checkbox" bind:checked={s.update_tools} />
						Update yt-dlp and deno when a new version is out (checked once a day)
					</label>
					<p class="small">Downloaded from the projects' official GitHub releases and checked against their published SHA-256.</p>
					{#if toolError}<p class="error">{toolError}</p>{/if}
				{:else}
					<p class="muted">Checking…</p>
				{/if}
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
				<div class="update">
					<label class="check">
						<input type="checkbox" bind:checked={s.check_updates} />
						Look for a new version of the app once a day
					</label>
					<button type="button" onclick={checkNow}>Check now</button>
				</div>
				{#if updateStatus}<p class="small">{updateStatus}</p>{/if}
				<label class="check">
					<input type="checkbox" bind:checked={s.watch_clipboard} />
					Offer download links I copy (archives, programs, video, music, documents)
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
	.login-row {
		display: grid;
		grid-template-columns: 1fr 1fr auto;
		gap: 8px;
		align-items: center;
		min-height: 30px;
	}
	.days {
		display: flex;
		flex-wrap: wrap;
		gap: 4px 12px;
		margin-top: 6px;
	}
	.day {
		display: flex;
		align-items: center;
		gap: 4px;
	}
	.update {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
	}
	.tool {
		display: grid;
		grid-template-columns: 70px 1fr auto;
		gap: 8px;
		align-items: center;
		min-height: 30px;
	}
	.small {
		font-size: 12px;
		color: var(--muted);
		margin: 6px 0 0;
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
