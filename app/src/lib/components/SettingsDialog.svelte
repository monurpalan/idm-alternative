<script lang="ts">
	import { onMount } from 'svelte';
	import { open as pickFile, open as pickFolder, save as pickSaveFile } from '@tauri-apps/plugin-dialog';
	import { Button, Checkbox, Dialog, Field, Input, Select, Text } from './ui/index.ts';
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
		type ToolId,
		type ToolStatus
	} from '../api.ts';
	import { msg, t } from '../i18n.svelte.ts';

	let { onclose }: { onclose: () => void } = $props();

	const TABS = [
		{ id: 'downloads', label: msg('Downloads') },
		{ id: 'connection', label: msg('Connection') },
		{ id: 'schedule', label: msg('Schedule') },
		{ id: 'logins', label: msg('Sign-ins') },
		{ id: 'tools', label: msg('Video tools') },
		{ id: 'system', label: msg('Browser and system') },
		{ id: 'app', label: msg('App') }
	] as const;
	let tab = $state<(typeof TABS)[number]['id']>('downloads');

	const DAYS = [msg('Mon'), msg('Tue'), msg('Wed'), msg('Thu'), msg('Fri'), msg('Sat'), msg('Sun')];

	const TOOLS: { id: ToolId; name: string; why: string }[] = [
		{ id: 'ytdlp', name: 'yt-dlp', why: msg('YouTube, X, Instagram and other sites') },
		{ id: 'deno', name: 'deno', why: msg('Needed by yt-dlp for YouTube') },
		{
			id: 'ffmpeg',
			name: 'ffmpeg',
			why: msg('Joins video and audio into one file; set up by itself when a download needs it')
		}
	];

	let s = $state<Settings | null>(null);
	/** The limit as typed, in KB/s; empty for none. */
	let limitKb = $state('');
	let error = $state<string | null>(null);
	let saving = $state(false);
	let dialog: { close(): void } | undefined = $state();
	let toolStatus = $state<ToolStatus | null>(null);
	let installing = $state<ToolId | null>(null);
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
				? t('Version {v} is ready: install it from the bar at the bottom of the window.', {
						v: u.version
					})
				: t('This is the latest version.');
		} catch (e) {
			updateStatus = t('Could not check: {e}', { e: String(e) });
		}
	}

	async function setUp(tool: ToolId) {
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
			defaultPath: 'shardload-settings.json',
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

	async function save() {
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

<Dialog bind:this={dialog} title={t('Settings')} size="wide" tall {onclose} onsubmit={save}>
	<div class="layout">
		<nav aria-label={t('Settings')}>
			{#each TABS as x (x.id)}
				<Button variant="nav" active={tab === x.id} onclick={() => (tab = x.id)}>{t(x.label)}</Button>
			{/each}
		</nav>

		<div class="page">
			{#if !s}
				{#if !error}<Text variant="muted">{t('Loading…')}</Text>{/if}
			{:else if tab === 'downloads'}
				<Field label={t('Save to')} layout="row">
					<Input bind:value={s.dir} />
					<Button onclick={browse}>{t('Browse…')}</Button>
				</Field>
				<Checkbox bind:checked={s.per_category}>{t('Sort into folders by type (Video, Music, Programs…)')}</Checkbox>
				<Checkbox bind:checked={s.overwrite}
					>{t('Replace files with the same name (otherwise save as “name (1)”)')}</Checkbox
				>
				<Checkbox bind:checked={s.find_checksums}
					>{t('Check downloads against the checksum the publisher posts (SHA256SUMS, GitHub)')}</Checkbox
				>
				<div class="grid">
					<Field label={t('Downloads at once')}>
						<Input type="number" min={1} max={50} bind:value={s.max_active} />
					</Field>
					<Field label={t('Connections per download')}>
						<Input type="number" min={1} max={64} bind:value={s.connections} />
					</Field>
					<Field label={t('Speed limit for all (KB/s)')}>
						<Input inputmode="numeric" bind:value={limitKb} placeholder={t('none')} />
					</Field>
				</div>
			{:else if tab === 'connection'}
				<div class="grid">
					<Field label={t('Introduce itself as')}>
						<Select bind:value={s.browser}>
							<option value="chrome">Chrome</option>
							<option value="edge">Edge</option>
							<option value="firefox">Firefox</option>
							<option value="safari">Safari</option>
							<option value="none">{t('Plain client')}</option>
						</Select>
					</Field>
					<div class="span2">
						<Field label={t('Proxy')}>
							<Input bind:value={s.proxy} placeholder={t('none, or socks5://host:port')} />
						</Field>
					</div>
				</div>
				<Text variant="caption">
					{t('Some servers only serve browsers: the app then sends the same headers as the one chosen.')}
				</Text>
			{:else if tab === 'schedule'}
				<Checkbox bind:checked={s.schedule.enabled}>{t('Start and stop the queue at set times')}</Checkbox>
				<div class="grid">
					<Field label={t('Start at')}>
						<Input type="time" bind:value={s.schedule.start} disabled={!s.schedule.enabled} />
					</Field>
					<Field label={t('Stop at')}>
						<Input type="time" bind:value={s.schedule.stop} disabled={!s.schedule.enabled} />
					</Field>
				</div>
				<div class="days" role="group" aria-label={t('Days')}>
					{#each DAYS as day, i (day)}
						<Checkbox bind:checked={s.schedule.days[i]} disabled={!s.schedule.enabled}>{t(day)}</Checkbox>
					{/each}
				</div>
				<Text variant="caption">
					{t(
						'Leave a time empty to only start or only stop. To shut the computer down after the queue, use “When all finish” in the side column.'
					)}
				</Text>
			{:else if tab === 'logins'}
				{#each logins as l (l.site)}
					<div class="row3">
						<span>{l.site}</span>
						<Text variant="muted" mono inline>{l.user}</Text>
						<Button onclick={() => forget(l.site)}>{t('Forget')}</Button>
					</div>
				{:else}
					<Text variant="muted">
						{t('None yet. Tick “Remember” when a server asks for a password.')}
					</Text>
				{/each}
			{:else if tab === 'tools'}
				{#if toolStatus}
					{#each TOOLS as tool (tool.id)}
						{@const version = toolStatus[tool.id]}
						{@const onPath = tool.id === 'ffmpeg' && toolStatus.ffmpeg_on_path}
						<div class="tool">
							<Text mono inline>{tool.name}</Text>
							<Text variant="muted" inline>
								{onPath
									? version
										? t('Yours, on PATH: version {v}', { v: version })
										: t('Yours, on PATH')
									: version
										? t('Version {v}', { v: version })
										: t(tool.why)}
							</Text>
							{#if !onPath}
								<Button disabled={installing !== null} onclick={() => setUp(tool.id)}>
									{installing === tool.id ? t('Downloading…') : version ? t('Update') : t('Set up')}
								</Button>
							{/if}
						</div>
					{/each}
					<Checkbox bind:checked={s.update_tools}
						>{t('Update yt-dlp, deno and ffmpeg when a new version is out (checked once a day)')}</Checkbox
					>
					<Text variant="caption">
						{t(
							"Downloaded from the projects' official GitHub releases and checked against their published SHA-256."
						)}
					</Text>
					{#if toolError}<Text variant="error">{toolError}</Text>{/if}
				{:else}
					<Text variant="muted">{t('Checking…')}</Text>
				{/if}
			{:else if tab === 'system'}
				<Checkbox bind:checked={s.browser_integration}
					>{t('Take downloads from the browser extension (Chrome, Edge, Brave, Vivaldi, Opera and Firefox)')}</Checkbox
				>
				<Checkbox bind:checked={s.watch_clipboard}
					>{t('Offer download links I copy (archives, programs, video, music, documents)')}</Checkbox
				>
				<Checkbox bind:checked={s.torrent_links}
					>{t('Open magnet links and .torrent files with this app (Windows may ask which app to use)')}</Checkbox
				>
				<Checkbox bind:checked={s.mark_downloads}
					>{t('Mark downloads as from the Internet, as browsers do (SmartScreen then checks them)')}</Checkbox
				>
				<Checkbox bind:checked={s.virus_scan}>{t('Scan finished downloads for viruses')}</Checkbox>
				<div class="indent">
					<Field label={t('Scanner')}>
						<Input
							bind:value={s.scanner}
							disabled={!s.virus_scan}
							placeholder={t('Microsoft Defender, or: "C:\\…\\scan.exe" {f}', { f: '{file}' })}
						/>
					</Field>
				</div>
			{:else if tab === 'app'}
				<div class="grid">
					<Field label={t('Language')}>
						<Select bind:value={s.language}>
							<option value="system">{t('Same as Windows')}</option>
							<option value="en">English</option>
							<option value="tr">Türkçe</option>
						</Select>
					</Field>
				</div>
				<Checkbox bind:checked={s.close_to_tray}>{t('Keep running in the tray when the window is closed')}</Checkbox>
				<Checkbox bind:checked={s.start_with_windows}>{t('Start with Windows (in the tray)')}</Checkbox>
				<Checkbox bind:checked={s.progress_windows}>{t('Show a small progress window for each download')}</Checkbox>
				<Checkbox bind:checked={s.notify}>{t('Notify me when a download finishes')}</Checkbox>
				<div class="spread">
					<Checkbox bind:checked={s.check_updates}>{t('Look for a new version once a day')}</Checkbox>
					<Button onclick={checkNow}>{t('Check now')}</Button>
				</div>
				{#if updateStatus}<Text variant="caption">{updateStatus}</Text>{/if}
				<div class="buttons">
					<Button onclick={exportFile}>{t('Export settings…')}</Button>
					<Button onclick={importFile}>{t('Import settings…')}</Button>
					<Button onclick={() => openLogs().catch((e) => (fileStatus = String(e)))}
						>{t('Open the log folder')}</Button
					>
				</div>
				{#if fileStatus}<Text variant="caption">{fileStatus}</Text>{/if}
			{/if}

			{#if error}<Text variant="error">{error}</Text>{/if}
		</div>
	</div>

	{#snippet actions()}
		<Button onclick={() => dialog?.close()}>{t('Cancel')}</Button>
		<Button type="submit" variant="primary" disabled={!s || saving}>{t('Save')}</Button>
	{/snippet}
</Dialog>

<style>
	.layout {
		display: grid;
		grid-template-columns: 176px 1fr;
		gap: var(--space-6);
		height: 100%;
		min-height: 0;
	}
	nav {
		display: flex;
		flex-direction: column;
		margin-left: calc(-1 * var(--space-6));
		border-right: 1px solid var(--line);
	}
	.page {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		min-height: 0;
		padding-right: var(--space-1);
		overflow-y: auto;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(3, 1fr);
		align-items: end;
		gap: var(--space-3);
	}
	.span2 {
		grid-column: span 2;
	}
	.days {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-1) var(--space-3);
	}
	.row3 {
		display: grid;
		grid-template-columns: 1fr 1fr auto;
		align-items: center;
		gap: var(--space-2);
	}
	.tool {
		display: grid;
		grid-template-columns: 72px 1fr auto;
		align-items: center;
		gap: var(--space-2);
		min-height: var(--space-8);
	}
	.spread {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-2);
	}
	.indent {
		padding-left: var(--space-6);
	}
	.buttons {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
		margin-top: var(--space-2);
	}
</style>
