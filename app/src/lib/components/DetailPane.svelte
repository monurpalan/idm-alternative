<script lang="ts">
	import { Button, Checkbox, Input, Text } from './ui/index.ts';
	// Details of one download: where it goes, live numbers, a speed graph, the segment map
	// (the engine's live splitting, one bar per connection's range), its own speed limit and
	// "refresh link" for links that expired.

	import {
		refreshInBrowser,
		refreshLink,
		setLogin,
		setSpeedLimit,
		siteMemory,
		type SiteMemory,
		type Task
	} from '../api.ts';
	import { downloads, HISTORY, moveToFolder, row } from '../downloads.svelte.ts';
	import { bytes, date, duration, percent, siteFacts, speed } from '../format.ts';
	import { t } from '../i18n.svelte.ts';

	let {
		task,
		onerror,
		onclose
	}: { task: Task; onerror: (message: string | null) => void; onclose: () => void } = $props();

	const r = $derived(row(task, downloads.live[task.id]));

	// A program dressed as a document or a video: `report.pdf.exe`, `clip.mp4.scr`.
	const SHOWN_AS =
		/\.(pdf|docx?|xlsx?|pptx?|txt|rtf|odt|jpe?g|png|gif|webp|mp3|wav|flac|mp4|mkv|avi|mov|webm|zip|rar|7z)\.(exe|scr|com|pif|bat|cmd|msi|vbs|vbe|js|jse|wsf|hta|cpl|lnk|ps1)$/i;
	const disguised = $derived(task.status === 'done' && SHOWN_AS.test(r.name));

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

	// Speed graph: redrawn with every new sample (reading `live` subscribes to it).
	$effect(() => {
		void downloads.live[task.id];
		if (!graph) return;
		const { ctx, w, h } = prepare(graph);
		const samples = downloads.history.get(task.id) ?? [];
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
		const x = (i: number) => w - ((samples.length - 1 - i) / (HISTORY - 1)) * w;
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
			<dt>{t('Downloaded')}</dt>
			<dd class="num">
				{r.size ? t('{a} of {b}', { a: bytes(r.received), b: bytes(r.size) }) : bytes(r.received)}{r.fraction !==
				null
					? ` · ${percent(r.fraction)}`
					: ''}
			</dd>
			<dt>{t('Speed')}</dt>
			<dd class="num">{speed(r.speed) || '–'}{r.eta !== null ? ` · ${t('{time} left', { time: duration(r.eta) })}` : ''}</dd>
			{#if learned}
				<dt>{t('Site')}</dt>
				<dd title={site?.site}>{t('This site {facts} (learned)', { facts: learned })}</dd>
			{/if}
			{#if task.error && task.status === 'done'}
				<!-- Said about a finished download that went differently than asked, e.g. saved as
				     one file without ffmpeg: worth knowing, not a failure. -->
				<dt>{t('Note')}</dt>
				<dd class="note" title={task.error}>{task.error}</dd>
			{:else if task.error}
				<dt>{t('Problem')}</dt>
				<dd class="error" title={task.error}>{task.error}</dd>
			{/if}
			{#if disguised}
				<dt>{t('Warning')}</dt>
				<dd class="error">{t('A program named like a document: open it only if you trust where it came from')}</dd>
			{/if}
		</dl>
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
			<form class="inline two" class:attention={task.error_kind === 'expired'} onsubmit={refresh}>
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
		<Text variant="label">{t('Speed, last minute')}</Text>
		<canvas bind:this={graph} class="graph" aria-label={t('Speed, last minute')}></canvas>
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
