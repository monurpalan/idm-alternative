<script lang="ts">
	import { Button, Checkbox, Input, Text, Textarea } from './ui/index.ts';
	import AfterLog from './AfterLog.svelte';
	import DeadlineFact from './DeadlineFact.svelte';
	import SkeletonFact from './SkeletonFact.svelte';
	import SlowHints from './SlowHints.svelte';
	import StructureFact from './StructureFact.svelte';
	import ArchiveFact from './ArchiveFact.svelte';
	import { untrack } from 'svelte';
	// Details of one download: where it goes, live numbers, a graph of its speed from its
	// first byte to its end, the segment map (the engine's live splitting, one bar per
	// connection's range), its own speed limit and "refresh link" for links that expired.

	import {
		refreshInBrowser,
		refreshLink,
		headerLines,
		setHeaders,
		setLogin,
		setSpeedLimit,
		pushSpeed,
		siteMemory,
		speedHistory,
		startTasks,
		type SiteMemory,
		type SpeedHistory,
		type Task
	} from '../api.ts';
	import { downloads, moveToFolder, row } from '../downloads.svelte.ts';
	import { algorithmName, bytes, date, duration, percent, siteFacts, speed, timeLeft } from '../format.ts';
	import { t } from '../i18n.svelte.ts';

	let {
		task,
		onerror,
		onclose
	}: { task: Task; onerror: (message: string | null) => void; onclose: () => void } = $props();

	const r = $derived(row(task, downloads.live[task.id]));

	// A program dressed as a document or a video, by its name (`report.pdf.exe`) or by its
	// bytes; or a web page (a sign-in page, an error) saved where the name promised a file.
	const disguised = $derived(task.status === 'done' && task.warning?.kind === 'disguised');
	const webPage = $derived(task.status === 'error' && task.warning?.kind === 'webpage');
	/** Checked against a checksum read off the page: a weaker promise than a publisher's. */
	const fromPage = $derived(task.checksum_from === 'page' && task.checksum !== null);

	/** What earlier downloads learned about the download's site. */
	let site = $state<SiteMemory | null>(null);
	$effect(() => {
		const url = task.url;
		void task.status; // a download that ends may have taught something
		siteMemory(url)
			.then((m) => {
				if (url === task.url) site = m;
			})
			.catch(() => (site = null));
	});
	const learned = $derived(site ? siteFacts(site).join(' · ') : '');
	/** Set when the link, by how long the site's links lasted, is about to expire. */
	const link = $derived(downloads.links[task.id]);

	let editingHeaders = $state(false);
	let headersText = $state('');
	// Another download selected: its own headers, not the edit of the last one.
	$effect(() => {
		void task.id;
		editingHeaders = false;
	});
	async function saveHeaders(e: SubmitEvent) {
		e.preventDefault();
		try {
			await setHeaders(task.id, headersText);
			editingHeaders = false;
			onerror(null);
		} catch (err) {
			onerror(String(err));
		}
	}

	let limitKb = $state('');
	let newUrl = $state('');
	let refreshing = $state(false);
	let user = $state('');
	let password = $state('');
	let remember = $state(false);

	async function signIn(e: SubmitEvent) {
		e.preventDefault();
		try {
			await setLogin(task.id, { user, password, remember });
			password = '';
			onerror(null);
		} catch (err) {
			onerror(String(err));
		}
	}
	let graph: HTMLCanvasElement | undefined = $state();
	let map: HTMLCanvasElement | undefined = $state();

	// Follow the selected download's stored limit.
	$effect(() => {
		limitKb = task.speed_limit ? String(Math.round(task.speed_limit / 1024)) : '';
	});

	async function applyLimit(e: SubmitEvent) {
		e.preventDefault();
		const kb = Number(limitKb.trim());
		try {
			await setSpeedLimit(task.id, limitKb.trim() && kb > 0 ? Math.round(kb * 1024) : null);
			onerror(null);
		} catch (err) {
			onerror(String(err));
		}
	}

	async function refresh(e: SubmitEvent) {
		e.preventDefault();
		if (!newUrl.trim()) return;
		refreshing = true;
		try {
			await refreshLink(task.id, newUrl);
			newUrl = '';
			onerror(null);
		} catch (err) {
			onerror(t('Link not changed: {e}', { e: String(err) }));
		} finally {
			refreshing = false;
		}
	}

	function token(name: string): string {
		return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
	}

	/** One of the three base colors (`--fg`, `--accent`, `--bg`) at an opacity. */
	function paint(ctx: CanvasRenderingContext2D, name: '--fg' | '--accent' | '--bg', alpha = 1) {
		ctx.fillStyle = token(name);
		ctx.strokeStyle = token(name);
		ctx.globalAlpha = alpha;
	}

	/** Canvas sized to its box in device pixels; returns the context and CSS size. */
	function prepare(c: HTMLCanvasElement) {
		const dpr = window.devicePixelRatio || 1;
		const w = c.clientWidth;
		const h = c.clientHeight;
		if (c.width !== Math.round(w * dpr) || c.height !== Math.round(h * dpr)) {
			c.width = Math.round(w * dpr);
			c.height = Math.round(h * dpr);
		}
		const ctx = c.getContext('2d')!;
		ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
		ctx.clearRect(0, 0, w, h);
		return { ctx, w, h };
	}

	/** The download's speed from its first byte on: the manager's, then a second at a time
	 * from the live numbers while it runs. Only the shown download's is held here. */
	let speeds = $state.raw<SpeedHistory | null>(null);
	/** The download `speeds` is of. */
	let speedsOf = 0;
	let sampledAt = 0;
	// Asked again when another download is shown, and when it starts or stops: the manager
	// has it whole then.
	$effect(() => {
		const id = task.id;
		void task.status;
		void task.started_at;
		if (id !== speedsOf) {
			speedsOf = id;
			speeds = null;
		}
		speedHistory(id)
			.then((h) => {
				if (id === task.id) speeds = h;
			})
			.catch(() => (speeds = null));
	});
	$effect(() => {
		const p = downloads.live[task.id];
		if (task.status !== 'downloading' || !p || (p.phase !== 'downloading' && p.phase !== 'recording')) return;
		const now = Date.now();
		if (now - sampledAt < 1000) return;
		sampledAt = now;
		untrack(() => {
			speeds = pushSpeed(speeds ?? { step: 1, points: [], count: 0, sum: 0 }, p.speed);
		});
	});

	// Speed graph: the whole download, oldest at the left; redrawn with every new second.
	$effect(() => {
		if (!graph) return;
		const { ctx, w, h } = prepare(graph);
		const samples = speeds ? [...speeds.points] : [];
		if (speeds?.count) samples.push(speeds.sum / speeds.count);
		const max = Math.max(1, ...samples);
		paint(ctx, '--fg', 0.09);
		ctx.lineWidth = 1;
		for (const f of [0.25, 0.5, 0.75]) {
			ctx.beginPath();
			ctx.moveTo(0, Math.round(h * f) + 0.5);
			ctx.lineTo(w, Math.round(h * f) + 0.5);
			ctx.stroke();
		}
		if (samples.length < 2) return;
		const x = (i: number) => (i / (samples.length - 1)) * w;
		const y = (v: number) => h - 2 - (v / max) * (h - 6);
		ctx.beginPath();
		ctx.moveTo(x(0), h);
		samples.forEach((v, i) => ctx.lineTo(x(i), y(v)));
		ctx.lineTo(x(samples.length - 1), h);
		ctx.closePath();
		paint(ctx, '--accent', 0.22);
		ctx.fill();
		ctx.beginPath();
		samples.forEach((v, i) => (i ? ctx.lineTo(x(i), y(v)) : ctx.moveTo(x(i), y(v))));
		paint(ctx, '--accent');
		ctx.lineWidth = 1.5;
		ctx.stroke();
		paint(ctx, '--fg', 0.62);
		ctx.font = `${token('--text-caption')} ${token('--font-mono')}`;
		ctx.fillText(speed(max), 4, 12);
		ctx.globalAlpha = 1;
	});

	// Segment map: each connection's range, filled as far as it got.
	$effect(() => {
		const live = downloads.live[task.id];
		const status = task.status;
		const size = r.size;
		const received = r.received;
		if (!map) return;
		const { ctx, w, h } = prepare(map);
		paint(ctx, '--fg', 0.1);
		ctx.fillRect(0, 0, w, h);
		if (status === 'done') {
			paint(ctx, '--fg', 0.26);
			ctx.fillRect(0, 0, w, h);
			ctx.globalAlpha = 1;
			return;
		}
		if (!size) return;
		const px = (b: number) => (b / size) * w;
		if (!live || live.segments.length === 0) {
			// Not running: the engine's map is not loaded; show how much is on disk.
			paint(ctx, '--fg', 0.3);
			ctx.fillRect(0, 0, px(received), h);
			ctx.globalAlpha = 1;
			return;
		}
		for (const [start, end, got, active] of live.segments) {
			paint(ctx, '--accent', active ? 1 : 0.55);
			ctx.fillRect(px(start), 0, Math.max(px(start + got) - px(start), got ? 1 : 0), h);
			// Boundary between ranges.
			paint(ctx, '--bg');
			ctx.fillRect(Math.floor(px(start)), 0, 1, h);
			if (active && got < end - start) {
				// Where this connection is writing now.
				paint(ctx, '--fg');
				ctx.fillRect(Math.min(w - 1, px(start + got)), 0, 1, h);
			}
		}
	});
</script>

<section class="pane" aria-label={t('Download details')}>
	<header>
		<h2 title={r.name}>{r.name}</h2>
		<Button variant="ghost" icon="close" onclick={onclose} label={t('Close the details')} title="Esc" />
	</header>
	<div class="facts">
		<dl>
			<dt>{t('Link')}</dt>
			<dd title={task.url}>{task.url}</dd>
			<dt>{t('Saved to')}</dt>
			<dd class="place">
				<span title={task.path ?? task.dir}
					>{downloads.moving[task.id] !== undefined
						? t('Moving {p}%', { p: Math.floor(downloads.moving[task.id] * 100) })
						: (task.path ?? task.dir)}</span
				>
				<Button
					variant="link"
					onclick={() => moveToFolder(task).then(() => onerror(null), (e) => onerror(String(e)))}
					disabled={downloads.moving[task.id] !== undefined}
					title={t('Move to another folder')}>{t('Move…')}</Button
				>
			</dd>
			<dt>{t('Added')}</dt>
			<dd class="num">{date(task.added_at)}</dd>
			{#if task.started_at !== null}
				<dt>{t('Started')}</dt>
				<dd class="num">{date(task.started_at)}</dd>
			{/if}
			{#if task.finished_at !== null}
				<dt>{t('Completed')}</dt>
				<dd class="num">
					{date(task.finished_at)}{task.started_at !== null
						? ` · ${duration((task.finished_at - task.started_at) / 1000)}`
						: ''}
				</dd>
			{/if}
			<dt>{t('Downloaded')}</dt>
			<dd class="num">
				{r.size ? t('{a} of {b}', { a: bytes(r.received), b: bytes(r.size) }) : bytes(r.received)}{r.fraction !==
				null
					? ` · ${percent(r.fraction)}`
					: ''}
			</dd>
			<dt>{t('Speed')}</dt>
			<dd class="num">{speed(r.speed) || '–'}{r.left ? ` · ${t('{time} left', { time: timeLeft(r.left) })}` : r.calculating ? ` · ${t('calculating')}` : ''}</dd>
			{#if r.live?.hints?.length && task.status === 'downloading'}
				<SlowHints id={task.id} hints={r.live.hints} {onerror} />
			{/if}
			{#if task.skeleton}
				<SkeletonFact skeleton={task.skeleton} />
			{/if}
			<DeadlineFact {task} {onerror} />
			{#if task.after_log.length}
				<AfterLog log={task.after_log} />
			{/if}
			{#if learned}
				<dt>{t('Site')}</dt>
				<dd title={site?.site}>{t('This site {facts} (learned)', { facts: learned })}</dd>
			{/if}
			<dt>{t('Request headers')}</dt>
			<dd class="wrap">
				{task.headers.length ? task.headers.map(([k]) => k).join(', ') : t('none')}
				<Button
					variant="link"
					onclick={() => {
						headersText = headerLines(task.headers);
						editingHeaders = !editingHeaders;
					}}>{editingHeaders ? t('Cancel') : t('Edit…')}</Button
				>
			</dd>
			{#if link}
				<dt>{t('Link')}</dt>
				<dd class:error={link.expired}>
					{link.expired
						? t('Probably expired, by how long links of this site lasted: get a fresh one below')
						: t('Expires in about {time}, by how long links of this site lasted', {
								time: duration(Math.max(60, (link.expires_at - Date.now()) / 1000))
							})}
					{#if !link.expired}
						<Button variant="link" onclick={() => startTasks([task.id]).catch((e) => onerror(String(e)))}
							>{t('Start now')}</Button
						>
					{/if}
				</dd>
			{/if}
			{#if task.error && task.status === 'done'}
				<!-- Said about a finished download that went differently than asked, e.g. saved as
				     one file without ffmpeg: worth knowing, not a failure. -->
				<dt>{t('Note')}</dt>
				<dd class="note" title={task.error}>{task.error}</dd>
			{:else if task.error_kind === 'disk' && task.status === 'paused'}
				<dt>{t('Problem')}</dt>
				<dd class="error wrap" title={task.error ?? ''}>
					{t('Paused before the drive filled up: free some space or move it to another drive, then resume.')}
				</dd>
			{:else if fromPage && task.error_kind === 'checksum'}
				<dt>{t('Problem')}</dt>
				<dd class="error wrap" title={task.error ?? ''}>
					{t('The file does not match the {algorithm} checksum shown on the page: the page may be out of date, or the file not the one it describes.', {
						algorithm: algorithmName(task.checksum ?? '')
					})}
				</dd>
			{:else if webPage}
				<dt>{t('Problem')}</dt>
				<dd class="error wrap" title={task.error}>
					{t('The server sent a web page, not the file: often a sign-in page or an expired link. Get a fresh link below.')}
				</dd>
			{:else if task.error}
				<dt>{t('Problem')}</dt>
				<dd class="error" title={task.error}>{task.error}</dd>
			{/if}
			{#if fromPage && task.status === 'done'}
				<dt>{t('Checksum')}</dt>
				<dd title={task.checksum ?? ''}>
					{t('Matches the {algorithm} checksum shown on the page', { algorithm: algorithmName(task.checksum ?? '') })}
				</dd>
			{/if}
			{#if task.structure && task.status === 'done'}
				<StructureFact structure={task.structure} />
			{/if}
			{#if task.error_kind === 'missing' && task.status === 'error'}
				<ArchiveFact id={task.id} {onerror} />
			{/if}
			{#if disguised}
				<dt>{t('Warning')}</dt>
				<dd class="error wrap">
					{task.warning?.by === 'content'
						? t('Its content is a program, though its name says otherwise: open it only if you trust where it came from')
						: t('A program named like a document: open it only if you trust where it came from')}
					<Text variant="caption" inline
						>{task.warning?.by === 'content' ? t('Found by its content') : t('Found by its name')}</Text
					>
				</dd>
			{/if}
		</dl>
		{#if editingHeaders}
			<form class="headers" onsubmit={saveHeaders}>
				<Textarea bind:value={headersText} rows={4} placeholder="Cookie: sid=…" />
				<span>
					<Button type="submit" variant="primary">{t('Save headers')}</Button>
					<Text variant="caption" inline>{t('Kept encrypted; a running download continues with them')}</Text>
				</span>
			</form>
		{/if}
		<form class="inline" onsubmit={applyLimit}>
			<label for="limit">{t('Speed limit')}</label>
			<Input id="limit" inputmode="numeric" bind:value={limitKb} placeholder={t('none')} />
			<Text variant="caption" mono inline>KB/s</Text>
			<Button type="submit">{t('Apply')}</Button>
		</form>
		{#if task.error_kind === 'auth'}
			<form class="inline login" onsubmit={signIn}>
				<label for="user">{t('Sign in')}</label>
				<Input id="user" bind:value={user} placeholder={t('user name')} autocomplete="username" />
				<Input
					type="password"
					bind:value={password}
					placeholder={t('password')}
					autocomplete="current-password"
					label={t('Password')}
				/>
				<span title={t('Downloads from this site sign in by themselves')}>
					<Checkbox bind:checked={remember}>{t('Remember')}</Checkbox>
				</span>
				<Button type="submit" variant="primary" disabled={!user}>{t('Sign in')}</Button>
			</form>
		{/if}
		{#if task.status !== 'done'}
			<form class="inline two" class:attention={task.error_kind === 'expired' || task.error_kind === 'missing' || link?.expired} onsubmit={refresh}>
				<label for="newurl">{t('New link')}</label>
				<Input id="newurl" type="url" bind:value={newUrl} placeholder={t('A fresh link to the same file')} />
				<Button type="submit" disabled={refreshing || !newUrl.trim()}>{t('Use it')}</Button>
			</form>
			<div class="inline two">
				<span></span>
				<span>
					<Button
						variant="link"
						onclick={() =>
							refreshInBrowser(task.id)
								.then(() => onerror(t('Waiting for the browser: start the download again on the page')))
								.catch((e) => onerror(String(e)))}>{t('Get a fresh link from the browser')}</Button
					>
				</span>
			</div>
		{/if}
	</div>
	<div class="charts">
		<Text variant="label">{t('Speed over the download')}</Text>
		<canvas bind:this={graph} class="graph" aria-label={t('Speed over the download')}></canvas>
		<Text variant="label">
			{t('Segments')} · {t('{a} of {b} connections', { a: r.live?.connections ?? 0, b: task.connections })}
		</Text>
		<canvas bind:this={map} class="map" aria-label={t('Segments')}></canvas>
	</div>
</section>

<style>
	.pane {
		box-sizing: border-box;
		display: grid;
		grid-template-columns: minmax(320px, 1fr) minmax(256px, 1fr);
		grid-template-rows: auto 1fr;
		column-gap: var(--space-6);
		row-gap: var(--space-3);
		height: 100%;
		padding: var(--space-3) var(--space-2) var(--space-4) var(--space-4);
		border-top: 1px solid var(--line-strong);
		background: var(--panel);
		overflow: hidden;
	}
	header {
		grid-column: 1 / -1;
		display: flex;
		align-items: center;
		gap: var(--space-2);
		min-width: 0;
	}
	h2 {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		font-size: var(--text-h2);
		font-weight: 600;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	/* Problem, sign-in and new-link rows can outgrow the pane: scroll rather than cut off. */
	.facts {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		min-height: 0;
		overflow-y: auto;
	}
	.facts > * {
		flex: none;
	}
	dl {
		display: grid;
		grid-template-columns: 96px 1fr;
		gap: var(--space-1) var(--space-2);
		margin: 0;
	}
	dt,
	.inline label {
		color: var(--muted);
		font-family: var(--font-mono);
		font-size: var(--text-caption);
		letter-spacing: 0.06em;
		line-height: 19px;
		text-transform: uppercase;
	}
	dd {
		margin: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.place {
		display: flex;
		align-items: baseline;
		gap: var(--space-2);
	}
	.place > span {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.place > :global(button) {
		flex: none;
	}
	.num {
		font-family: var(--font-mono);
		font-variant-numeric: tabular-nums;
	}
	.error {
		color: var(--danger);
	}
	.note {
		color: var(--muted);
	}
	.headers {
		display: grid;
		gap: var(--space-2);
		margin-bottom: var(--space-3);
	}
	/* A warning is read whole, not from a tooltip. */
	.wrap {
		white-space: normal;
	}
	.inline {
		display: grid;
		grid-template-columns: 96px 1fr auto auto;
		align-items: center;
		gap: var(--space-2);
	}
	.inline.two {
		grid-template-columns: 96px 1fr auto;
	}
	.inline.login {
		grid-template-columns: 96px 1fr 1fr auto auto;
	}
	.attention label {
		color: var(--danger);
	}
	.charts {
		display: grid;
		grid-template-rows: auto 1fr auto var(--space-6);
		gap: var(--space-1);
		min-width: 0;
		min-height: 0;
		padding-right: var(--space-2);
	}
	canvas {
		box-sizing: border-box;
		width: 100%;
		height: 100%;
		min-height: 0;
		border: 1px solid var(--line);
		background: var(--bg);
	}
</style>
