<script lang="ts">
	import { onMount } from 'svelte';
	import { open as pickFile, open as pickFolder, save as pickSaveFile } from '@tauri-apps/plugin-dialog';
	import {
		getSettings,
		checkUpdate,
		exportSettings,
		importSettings,
		openLogs,
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
	import { msg, t } from '../i18n.svelte.ts';

	let { onclose }: { onclose: () => void } = $props();

	const DAYS = [msg('Mon'), msg('Tue'), msg('Wed'), msg('Thu'), msg('Fri'), msg('Sat'), msg('Sun')];

	const TOOLS: { id: 'ytdlp' | 'deno'; name: string; why: string }[] = [
		{ id: 'ytdlp', name: 'yt-dlp', why: msg('YouTube, X, Instagram and other sites') },
		{ id: 'deno', name: 'deno', why: msg('Needed by yt-dlp for YouTube') }
	];

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
		updateStatus = t('Checking…');
		try {
			const u = await checkUpdate();
			updateStatus = u
				? t('Version {v} is available: install it from the bar at the bottom of the window.', {
						v: u.version
					})
				: t('This is the latest version.');
		} catch (e) {
			updateStatus = t('Could not check: {e}', { e: String(e) });
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
				.then((v) => (toolStatus = v))
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

	/** What came of exporting or importing, shown under the buttons. */
	let fileStatus = $state<string | null>(null);

	const JSON_FILES = [{ name: 'JSON', extensions: ['json'] }];

	async function exportFile() {
		const path = await pickSaveFile({
			defaultPath: 'idm-alternative-settings.json',
			filters: JSON_FILES
		});
		if (!path) return;
		try {
			await exportSettings(path);
			fileStatus = t('Settings saved to {path}', { path });
		} catch (e) {
			fileStatus = t('Could not save the settings: {e}', { e: String(e) });
		}
	}

	async function importFile() {
		const path = await pickFile({ filters: JSON_FILES });
		if (typeof path !== 'string') return;
		try {
			const v = await importSettings(path);
			s = v;
			limitKb = v.speed_limit ? String(Math.round(v.speed_limit / 1024)) : '';
			fileStatus = t('Settings loaded and applied');
		} catch (e) {
			fileStatus = t('Could not load the settings: {e}', { e: String(e) });
		}
	}

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
			error = t('The speed limit must be a positive number of KB/s, or empty.');
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
		<h2 id="settings-title">{t('Settings')}</h2>
		{#if s}
			<fieldset>
				<legend>{t('Downloads')}</legend>
				<label class="wide">
					{t('Save to')}
					<div class="row">
						<input bind:value={s.dir} />
						<button type="button" onclick={browse}>{t('Browse…')}</button>
					</div>
				</label>
				<label class="check">
					<input type="checkbox" bind:checked={s.per_category} />
					{t('Sort into folders by type (Video, Music, Programs…)')}
				</label>
				<label class="check">
					<input type="checkbox" bind:checked={s.overwrite} />
					{t('Replace files with the same name (otherwise save as “name (1)”)')}
				</label>
				<div class="grid">
					<label>
						{t('Downloads at once')}
						<input type="number" min="1" max="50" bind:value={s.max_active} />
					</label>
					<label>
						{t('Connections per download')}
						<input type="number" min="1" max="64" bind:value={s.connections} />
					</label>
					<label>
						{t('Speed limit, all downloads (KB/s)')}
						<input inputmode="numeric" bind:value={limitKb} placeholder={t('none')} />
					</label>
				</div>
			</fieldset>

			<fieldset>
				<legend>{t('Connection')}</legend>
				<div class="grid">
					<label>
						{t('Look like')}
						<select bind:value={s.browser}>
							<option value="chrome">Chrome</option>
							<option value="edge">Edge</option>
							<option value="firefox">Firefox</option>
							<option value="safari">Safari</option>
							<option value="none">{t('Plain client')}</option>
						</select>
					</label>
					<label class="span2">
						{t('Proxy')}
						<input bind:value={s.proxy} placeholder={t('none, or socks5://host:port')} />
					</label>
				</div>
			</fieldset>

			<fieldset>
				<legend>{t('Scheduler')}</legend>
				<label class="check">
					<input type="checkbox" bind:checked={s.schedule.enabled} />
					{t('Start and stop the queue at set times')}
				</label>
				<div class="grid">
					<label>
						{t('Start at')}
						<input type="time" bind:value={s.schedule.start} disabled={!s.schedule.enabled} />
					</label>
					<label>
						{t('Stop at')}
						<input type="time" bind:value={s.schedule.stop} disabled={!s.schedule.enabled} />
					</label>
				</div>
				<div class="days" role="group" aria-label={t('Days')}>
					{#each DAYS as day, i (day)}
						<label class="day">
							<input
								type="checkbox"
								bind:checked={s.schedule.days[i]}
								disabled={!s.schedule.enabled}
							/>
							{t(day)}
						</label>
					{/each}
				</div>
				<p class="small">
					{t(
						'Leave a time empty to only start or only stop. "When done" in the toolbar can shut the computer down after the queue finishes.'
					)}
				</p>
			</fieldset>

			<fieldset>
				<legend>{t('Saved logins')}</legend>
				{#each logins as l (l.site)}
					<div class="login-row">
						<span>{l.site}</span>
						<span class="muted">{l.user}</span>
						<button type="button" onclick={() => forget(l.site)}>{t('Forget')}</button>
					</div>
				{:else}
					<p class="muted">
						{t('None yet. Tick “Remember for this site” when a server asks for a password.')}
					</p>
				{/each}
			</fieldset>

			<fieldset>
				<legend>{t('Video tools')}</legend>
				{#if toolStatus}
					<div class="tool">
						<span>ffmpeg</span>
						<span class="muted">
							{toolStatus.ffmpeg
								? t('Found')
								: t('Not found: install it (e.g. winget install ffmpeg) to join video and audio')}
						</span>
					</div>
					{#each TOOLS as tool (tool.id)}
						{@const version = toolStatus[tool.id]}
						<div class="tool">
							<span>{tool.name}</span>
							<span class="muted">{version ? t('Version {v}', { v: version }) : t(tool.why)}</span>
							<button type="button" disabled={installing !== null} onclick={() => setUp(tool.id)}>
								{installing === tool.id ? t('Downloading…') : version ? t('Update') : t('Set up')}
							</button>
						</div>
					{/each}
					<label class="check">
						<input type="checkbox" bind:checked={s.update_tools} />
						{t('Update yt-dlp and deno when a new version is out (checked once a day)')}
					</label>
					<p class="small">
						{t(
							"Downloaded from the projects' official GitHub releases and checked against their published SHA-256."
						)}
					</p>
					{#if toolError}<p class="error">{toolError}</p>{/if}
				{:else}
					<p class="muted">{t('Checking…')}</p>
				{/if}
			</fieldset>

			<fieldset>
				<legend>{t('App')}</legend>
				<label class="language">
					{t('Language')}
					<select bind:value={s.language}>
						<option value="system">{t('Same as Windows')}</option>
						<option value="en">English</option>
						<option value="tr">Türkçe</option>
					</select>
				</label>
				<label class="check">
					<input type="checkbox" bind:checked={s.close_to_tray} />
					{t('Keep running in the tray when the window is closed')}
				</label>
				<label class="check">
					<input type="checkbox" bind:checked={s.start_with_windows} />
					{t('Start with Windows (in the tray)')}
				</label>
				<label class="check">
					<input type="checkbox" bind:checked={s.progress_windows} />
					{t('Show a small progress window for each download')}
				</label>
				<label class="check">
					<input type="checkbox" bind:checked={s.notify} />
					{t('Notify me when a download finishes')}
				</label>
				<div class="update">
					<label class="check">
						<input type="checkbox" bind:checked={s.check_updates} />
						{t('Look for a new version of the app once a day')}
					</label>
					<button type="button" onclick={checkNow}>{t('Check now')}</button>
				</div>
				{#if updateStatus}<p class="small">{updateStatus}</p>{/if}
				<label class="check">
					<input type="checkbox" bind:checked={s.virus_scan} />
					{t('Scan finished downloads for viruses')}
				</label>
				<label class="scanner">
					{t('Scanner')}
					<input
						bind:value={s.scanner}
						disabled={!s.virus_scan}
						placeholder={t('Microsoft Defender, or: "C:\\…\\scan.exe" {f}', { f: '{file}' })}
					/>
				</label>
				<label class="check">
					<input type="checkbox" bind:checked={s.watch_clipboard} />
					{t('Offer download links I copy (archives, programs, video, music, documents)')}
				</label>
				<label class="check">
					<input type="checkbox" bind:checked={s.torrent_links} />
					{t('Open magnet links and .torrent files with this app (Windows may ask which app to use)')}
				</label>
				<label class="check">
					<input type="checkbox" bind:checked={s.browser_integration} />
					{t(
						'Take downloads from the browser extension (registers with Chrome, Edge, Brave, Vivaldi, Opera and Firefox)'
					)}
				</label>
				<div class="files">
					<button type="button" onclick={exportFile}>{t('Export settings…')}</button>
					<button type="button" onclick={importFile}>{t('Import settings…')}</button>
					<button type="button" onclick={() => openLogs().catch((e) => (fileStatus = String(e)))}
						>{t('Open the log folder')}</button
					>
				</div>
				{#if fileStatus}<p class="small">{fileStatus}</p>{/if}
			</fieldset>
		{:else if !error}
			<p class="muted">{t('Loading…')}</p>
		{/if}

		{#if error}<p class="error">{error}</p>{/if}

		<div class="buttons">
			<button type="button" onclick={() => dialog?.close()}>{t('Cancel')}</button>
			<button type="submit" class="primary" disabled={!s || saving}>{t('Save')}</button>
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
	.language {
		align-items: flex-start;
		margin: 2px 0 8px;
	}
	.scanner {
		margin-left: 24px;
	}
	.login-row {
		display: grid;
		grid-template-columns: 1fr 1fr auto;
		gap: 8px;
		align-items: center;
		min-height: 30px;
	}
	.files {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
		margin-top: 6px;
	}
	.days {
		display: flex;
		flex-wrap: wrap;
		gap: 4px 12px;
		margin-top: 6px;
	}
	.day {
		display: flex;
		flex-direction: row;
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
