<script lang="ts">
	// Details of one download: where it goes, live numbers, a speed graph, the segment map
	// (the engine's live splitting, one bar per connection's range), its own speed limit and
	// "refresh link" for links that expired.

	import { refreshInBrowser, refreshLink, setLogin, setSpeedLimit, type Task } from '../api.ts';
	import { downloads, HISTORY, row } from '../downloads.svelte.ts';
	import { bytes, duration, percent, speed } from '../format.ts';
	import { t } from '../i18n.svelte.ts';

	let { task, onerror }: { task: Task; onerror: (message: string | null) => void } = $props();

	const r = $derived(row(task, downloads.live[task.id]));

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
		ctx.strokeStyle = token('--line-soft');
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
		ctx.fillStyle = token('--accent');
		ctx.globalAlpha = 0.22;
		ctx.fill();
		ctx.globalAlpha = 1;
		ctx.beginPath();
		samples.forEach((v, i) => (i ? ctx.lineTo(x(i), y(v)) : ctx.moveTo(x(i), y(v))));
		ctx.strokeStyle = token('--accent');
		ctx.lineWidth = 1.5;
		ctx.stroke();
		ctx.fillStyle = token('--muted');
		ctx.font = '11px system-ui, sans-serif';
		ctx.fillText(speed(max), 4, 12);
	});

	// Segment map: each connection's range, filled as far as it got.
	$effect(() => {
		const live = downloads.live[task.id];
		const status = task.status;
		const size = r.size;
		const received = r.received;
		if (!map) return;
		const { ctx, w, h } = prepare(map);
		ctx.fillStyle = token('--track');
		ctx.fillRect(0, 0, w, h);
		if (status === 'done') {
			ctx.fillStyle = token('--ok');
			ctx.fillRect(0, 0, w, h);
			return;
		}
		if (!size) return;
		const px = (b: number) => (b / size) * w;
		if (!live || live.segments.length === 0) {
			// Not running: the engine's map is not loaded; show how much is on disk.
			ctx.fillStyle = token('--muted');
			ctx.fillRect(0, 0, px(received), h);
			return;
		}
		for (const [start, end, got, active] of live.segments) {
			ctx.fillStyle = token('--accent');
			ctx.globalAlpha = active ? 1 : 0.55;
			ctx.fillRect(px(start), 0, Math.max(px(start + got) - px(start), got ? 1 : 0), h);
			ctx.globalAlpha = 1;
			// Boundary between ranges.
			ctx.fillStyle = token('--bg');
			ctx.fillRect(Math.floor(px(start)), 0, 1, h);
			if (active && got < end - start) {
				// Where this connection is writing now.
				ctx.fillStyle = token('--fg');
				ctx.fillRect(Math.min(w - 1, px(start + got)), 0, 1, h);
			}
		}
	});
</script>

<section class="pane" aria-label={t('Download details')}>
	<div class="facts">
		<h3 title={r.name}>{r.name}</h3>
		<dl>
			<dt>{t('Address')}</dt>
			<dd title={task.url}>{task.url}</dd>
			<dt>{t('Saved to')}</dt>
			<dd title={task.path ?? task.dir}>{task.path ?? task.dir}</dd>
			<dt>{t('Downloaded')}</dt>
			<dd>
				{r.size ? t('{a} of {b}', { a: bytes(r.received), b: bytes(r.size) }) : bytes(r.received)}{r.fraction !==
				null
					? ` (${percent(r.fraction)})`
					: ''}
			</dd>
			<dt>{t('Speed')}</dt>
			<dd>{speed(r.speed) || '–'}{r.eta !== null ? ` · ${t('{time} left', { time: duration(r.eta) })}` : ''}</dd>
			<dt>{t('Connections')}</dt>
			<dd>{t('{a} of {b}', { a: r.live?.connections ?? 0, b: task.connections })}</dd>
			{#if task.error}
				<dt>{t('Error')}</dt>
				<dd class="error" title={task.error}>{task.error}</dd>
			{/if}
		</dl>
		<form class="inline" onsubmit={applyLimit}>
			<label for="limit">{t('Speed limit')}</label>
			<input id="limit" inputmode="numeric" bind:value={limitKb} placeholder={t('none')} />
			<span class="unit">KB/s</span>
			<button type="submit">{t('Set')}</button>
		</form>
		{#if task.error_kind === 'auth'}
			<form class="inline login" onsubmit={signIn}>
				<label for="user">{t('Login')}</label>
				<input id="user" bind:value={user} placeholder={t('user name')} autocomplete="username" />
				<input
					type="password"
					bind:value={password}
					placeholder={t('password')}
					autocomplete="current-password"
					aria-label={t('Password')}
				/>
				<label class="remember" title={t('Downloads from this site log in by themselves')}>
					<input type="checkbox" bind:checked={remember} />
					{t('Remember')}
				</label>
				<button type="submit" disabled={!user}>{t('Continue')}</button>
			</form>
		{/if}
		{#if task.status !== 'done'}
			<form class="inline two" class:attention={task.error_kind === 'expired'} onsubmit={refresh}>
				<label for="newurl">{t('New link')}</label>
				<input id="newurl" type="url" bind:value={newUrl} placeholder={t('Paste a fresh link to the same file')} />
				<button type="submit" disabled={refreshing || !newUrl.trim()}>{t('Refresh')}</button>
			</form>
			<div class="inline two">
				<span></span>
				<span class="hint">{t('or open its page and start the download again there:')}</span>
				<button
					type="button"
					onclick={() =>
						refreshInBrowser(task.id)
							.then(() => onerror(t('Waiting for the browser: start the download again on the page')))
							.catch((e) => onerror(String(e)))}>{t('In browser')}</button
				>
			</div>
		{/if}
	</div>
	<div class="charts">
		<canvas bind:this={graph} class="graph" aria-label={t('Speed over the last minute')}></canvas>
		<canvas bind:this={map} class="map" aria-label={t('Segment map')}></canvas>
	</div>
</section>

<style>
	.pane {
		display: grid;
		grid-template-columns: minmax(320px, 1fr) minmax(260px, 1.1fr);
		gap: 16px;
		height: 100%;
		box-sizing: border-box;
		padding: 10px 12px;
		border-top: 1px solid var(--line);
		background: var(--panel);
		overflow: hidden;
	}
	/* Error, login and new-link rows can outgrow the pane: scroll rather than cut off. */
	.facts {
		min-height: 0;
		overflow-y: auto;
	}
	h3 {
		margin: 0 0 6px;
		font-size: 14px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	dl {
		display: grid;
		grid-template-columns: 92px 1fr;
		gap: 3px 8px;
		margin: 0 0 8px;
	}
	dt {
		color: var(--muted);
	}
	dd {
		margin: 0;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		font-variant-numeric: tabular-nums;
	}
	.error {
		color: var(--danger);
	}
	.inline {
		display: grid;
		grid-template-columns: 92px 1fr auto auto;
		align-items: center;
		gap: 4px 8px;
		margin-top: 4px;
	}
	.inline.two {
		grid-template-columns: 92px 1fr auto;
	}
	.inline.login {
		grid-template-columns: 92px 1fr 1fr auto auto;
	}
	.remember {
		display: flex;
		align-items: center;
		gap: 4px;
		white-space: nowrap;
	}
	.attention input {
		border-color: var(--accent);
	}
	.inline label {
		color: var(--muted);
	}
	.inline input {
		min-width: 0;
		padding: 4px 6px;
		border: 1px solid var(--line);
		border-radius: 4px;
		background: var(--field);
		color: var(--fg);
		font: inherit;
	}
	.inline button {
		padding: 3px 10px;
	}
	.unit,
	.hint {
		color: var(--muted);
	}
	.charts {
		display: grid;
		grid-template-rows: 1fr 22px;
		gap: 8px;
		min-width: 0;
		min-height: 0;
	}
	canvas {
		width: 100%;
		height: 100%;
		min-height: 0;
		border: 1px solid var(--line);
		border-radius: 4px;
		background: var(--bg);
		box-sizing: border-box;
	}
</style>
