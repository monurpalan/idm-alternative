// UI flow tests of the app on Windows: the debug build, its web UI served by Vite, in a profile
// of its own (SHARDLOAD_HOME in C:\tmp\shardload-ui-test), driven over the DevTools Protocol of its
// WebView2 like a user would: clicks, typing, a right click. Downloads come from the test
// server; a captured video arrives through the native host, as from the browser extension.
//
//   cargo build && cargo build -p shardload-app      (in the repository)
//   npm run ui-test                                   (in app/)
//
// Screenshots of a few views, light and dark, are compared with ui-test/baseline/ (a share of
// pixels may differ a little: anti-aliasing); `npm run ui-test -- --update` writes new ones,
// `-- --no-compare` only saves them (to ui-test/out/). WebView2 draws with the monitor's colour
// profile, so pictures from one machine do not fit another: CI keeps its own (CI=true, in
// ui-test/baseline-ci/). A running Shardload must be closed first: the app lets one copy run.

import { execFileSync, spawn } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, openSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { decode, difference } from './png.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const APP = resolve(HERE, '..');
const BIN = resolve(APP, '..', 'target', 'debug');
const exe = (name) => join(BIN, `${name}.exe`);
const SERVER_PORT = 8791;
const CDP_PORT = 9345;
const SERVER = `http://127.0.0.1:${SERVER_PORT}`;
/** The test profile: a fixed path, which the screenshots show (no user name in it). */
const HOME = 'C:\\tmp\\shardload-ui-test';
/** Of a view's pixels, the share that may differ from the baseline. */
const MAX_DIFFERENCE = 0.005;
const UPDATE = process.argv.includes('--update');
const COMPARE = !process.argv.includes('--no-compare');

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
/** Flows failed so far: screenshots taken after one are not made the baseline. */
let failures = 0;

async function waitFor(check, what, ms = 15_000) {
	const end = Date.now() + ms;
	while (Date.now() < end) {
		if (await check().catch(() => false)) return;
		await sleep(100);
	}
	throw new Error(`timed out waiting for ${what}`);
}

// ---- Programs -----------------------------------------------------------------------------

const started = [];

function start(file, args, options = {}) {
	const child = spawn(file, args, { stdio: 'ignore', ...options });
	started.push(child);
	return child;
}

/** Ends every program started here, with their children (Vite runs under npx's node). */
function stopAll() {
	for (const child of started) {
		if (child.exitCode !== null) continue;
		try {
			execFileSync('taskkill', ['/pid', String(child.pid), '/T', '/F'], { stdio: 'ignore' });
		} catch {
			// already gone
		}
	}
}

function running(image) {
	return execFileSync('tasklist', ['/FI', `IMAGENAME eq ${image}`, '/NH'], { encoding: 'utf8' }).includes(image);
}

/** The WebView2 processes' command lines, to see which switches reached them. */
function webviews() {
	try {
		return execFileSync(
			'powershell',
			['-NoProfile', '-Command', "Get-CimInstance Win32_Process -Filter \"Name='msedgewebview2.exe'\" | Where-Object { $_.CommandLine -notmatch '--type=' } | ForEach-Object { $_.CommandLine }"],
			{ encoding: 'utf8' }
		);
	} catch (e) {
		return String(e);
	}
}

/** Sends one message to the app through the native host, as the browser extension does. */
function fromBrowser(home, message) {
	const body = Buffer.from(JSON.stringify(message));
	const length = Buffer.alloc(4);
	length.writeUInt32LE(body.length);
	const reply = execFileSync(exe('shardload-host'), ['chrome-extension://ui-test/'], {
		input: Buffer.concat([length, body]),
		env: { ...process.env, SHARDLOAD_HOME: home, SHARDLOAD_APP: exe('shardload') }
	});
	return JSON.parse(reply.subarray(4).toString());
}

// ---- The DevTools Protocol ----------------------------------------------------------------

class Page {
	#ws;
	#id = 0;
	#pending = new Map();

	static async connect(url) {
		const page = new Page();
		page.#ws = new WebSocket(url);
		await new Promise((ok, fail) => {
			page.#ws.onopen = ok;
			page.#ws.onerror = fail;
		});
		page.#ws.onmessage = (e) => {
			const m = JSON.parse(e.data);
			const p = m.id !== undefined && page.#pending.get(m.id);
			if (!p) return;
			page.#pending.delete(m.id);
			if (m.error) p.fail(new Error(`${p.method}: ${m.error.message}`));
			else p.ok(m.result);
		};
		return page;
	}

	send(method, params = {}) {
		const id = ++this.#id;
		this.#ws.send(JSON.stringify({ id, method, params }));
		return new Promise((ok, fail) => this.#pending.set(id, { ok, fail, method }));
	}

	/** The value of `body`, the body of an async function in the page. */
	async run(body) {
		const r = await this.send('Runtime.evaluate', {
			expression: `(async () => { ${body} })()`,
			returnByValue: true,
			awaitPromise: true
		});
		if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? r.exceptionDetails.text);
		return r.result.value;
	}

	/** The centre of the first element matching `selector` (with `text` in it, if given). */
	async centre(selector, text) {
		const at = await this.run(`
			const el = [...document.querySelectorAll(${JSON.stringify(selector)})]
				.find((e) => ${JSON.stringify(text ?? '')} === '' || e.textContent.includes(${JSON.stringify(text ?? '')}));
			if (!el) return null;
			el.scrollIntoView({ block: 'nearest' });
			const r = el.getBoundingClientRect();
			return { x: r.x + r.width / 2, y: r.y + r.height / 2 };`);
		if (!at) throw new Error(`no ${selector}${text ? ` with "${text}"` : ''}`);
		return at;
	}

	async click(selector, text, button = 'left') {
		const { x, y } = await this.centre(selector, text);
		await this.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y });
		for (const type of ['mousePressed', 'mouseReleased']) {
			await this.send('Input.dispatchMouseEvent', { type, x, y, button, clickCount: 1 });
		}
	}

	async type(text) {
		await this.send('Input.insertText', { text });
	}

	async key(key, code, keyCode) {
		for (const type of ['rawKeyDown', 'keyUp']) {
			await this.send('Input.dispatchKeyEvent', { type, key, code, windowsVirtualKeyCode: keyCode });
		}
	}

	exists(selector, text = '') {
		return this.run(`return [...document.querySelectorAll(${JSON.stringify(selector)})]
			.some((e) => e.textContent.includes(${JSON.stringify(text)}));`);
	}

	text(selector) {
		return this.run(`return document.querySelector(${JSON.stringify(selector)})?.innerText ?? '';`);
	}

	close() {
		this.#ws.close();
	}
}

// ---- Screenshots --------------------------------------------------------------------------

/** Saves the view as `name` in light and dark and compares both with the baseline. */
async function shoot(page, name) {
	const problems = [];
	// No caret, focus ring or hover in the picture.
	await page.run('document.activeElement?.blur();');
	await page.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: 1, y: 1 });
	for (const scheme of ['light', 'dark']) {
		await page.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-color-scheme', value: scheme }] });
		await sleep(400);
		const { data } = await page.send('Page.captureScreenshot', { format: 'png' });
		const png = Buffer.from(data, 'base64');
		const file = `${name}-${scheme}.png`;
		mkdirSync(join(HERE, 'out'), { recursive: true });
		writeFileSync(join(HERE, 'out', file), png);
		const baseline = join(HERE, process.env.CI ? 'baseline-ci' : 'baseline', file);
		if ((UPDATE || (COMPARE && !existsSync(baseline))) && failures > 0) {
			problems.push(`${file}: not made the baseline after a failed flow`);
		} else if (UPDATE || (COMPARE && !existsSync(baseline))) {
			mkdirSync(dirname(baseline), { recursive: true });
			writeFileSync(baseline, png);
		} else if (COMPARE) {
			const share = difference(decode(png), decode(readFileSync(baseline)));
			if (share > MAX_DIFFERENCE) problems.push(`${file}: ${(share * 100).toFixed(2)} % of pixels differ`);
		}
	}
	await page.send('Emulation.setEmulatedMedia', { features: [] });
	if (problems.length) throw new Error(problems.join('; '));
}

// ---- The flows ----------------------------------------------------------------------------

const URL = `${SERVER}/file/report.pdf?size=6M&seed=3&rate=2M`;
const VIDEO = `${SERVER}/hls/master.m3u8?variants=360,720&audio=1&segments=6&seg_size=64K&seed=5`;

async function flows(page, home, test) {
	const row = 'tbody tr';
	const downloaded = join(home, 'downloads', 'report.pdf');

	await test('settings: every tab shows its own options; the language is set to English', async () => {
		await page.click('aside button.icon-only');
		await waitFor(() => page.exists('dialog[open] button.nav'), 'the settings');
		const tabs = await page.run(`return document.querySelectorAll('dialog[open] button.nav').length;`);
		if (tabs < 10) throw new Error(`${tabs} tabs`);
		const seen = new Set();
		for (let i = 0; i < tabs; i++) {
			await page.run(`document.querySelectorAll('dialog[open] button.nav')[${i}].click();`);
			// Its own options, once the tab is drawn (the settings load after the window opens).
			let shown = '';
			await waitFor(async () => {
				shown = await page.run(`
					if (!document.querySelectorAll('dialog[open] button.nav')[${i}].classList.contains('active')) return '';
					const body = document.querySelector('dialog[open]').cloneNode(true);
					body.querySelectorAll('button.nav').forEach((b) => b.remove());
					return body.innerText;`);
				return shown !== '' && !seen.has(shown);
			}, `tab ${i} to show its own options`);
			seen.add(shown);
			const language = await page.run(`return !!document.querySelector('dialog[open] select option[value="tr"]');`);
			if (language) {
				await page.run(`const s = [...document.querySelectorAll('dialog[open] select')].find((s) => s.querySelector('option[value="tr"]'));
					s.value = 'en'; s.dispatchEvent(new Event('change', { bubbles: true }));`);
			}
		}
		await page.click('dialog[open] button[type=submit]');
		await waitFor(async () => !(await page.exists('dialog[open]')), 'the settings to close');
		await waitFor(() => page.exists('header', 'All downloads'), 'English');
	});

	await test('a new download: the link asked about, started, finished', async () => {
		await page.click('aside button.primary');
		await waitFor(() => page.exists('dialog[open] input.field'), 'the dialog');
		await page.click('dialog[open] input.field');
		await page.type(URL);
		await page.key('Tab', 'Tab', 9);
		await waitFor(() => page.exists('dialog[open] p.info', '6.00 MB'), 'the server’s answer');
		const name = await page.run(`return document.querySelectorAll('dialog[open] input.field')[1].value;`);
		if (name !== 'report.pdf') throw new Error(`named ${name}`);
		await page.click('dialog[open] button[type=submit]');
		await waitFor(() => page.exists('tbody tr.s-done', 'report.pdf'), 'the download to finish', 60_000);
		execFileSync(exe('testserver'), ['verify', downloaded, '--size', '6M', '--seed', '3']);
	});

	await test('screenshots: the list, and the new download dialog', async () => {
		await page.key('Escape', 'Escape', 27);
		await shoot(page, 'list');
		await page.click('aside button.primary');
		await waitFor(() => page.exists('dialog[open] input.field'), 'the dialog');
		await shoot(page, 'new-download');
		await page.click('dialog[open] button.secondary', 'Cancel');
		await waitFor(async () => !(await page.exists('dialog[open]')), 'the dialog to close');
	});

	await test('the details of a download, and closing them', async () => {
		await page.click(row, 'report.pdf');
		await waitFor(() => page.exists('section.pane', 'report.pdf'), 'the details');
		const details = await page.text('section.pane');
		if (!details.includes(URL.slice(0, 40))) throw new Error('no link in the details');
		if (!details.includes('6.00 MB of 6.00 MB')) throw new Error('no size in the details');
		await page.click('section.pane button.icon-only');
		await waitFor(async () => !(await page.exists('section.pane')), 'the details to close');
	});

	await test('the context menu of a download', async () => {
		await page.click(row, 'report.pdf', 'right');
		await waitFor(() => page.exists('.menu[role=menu]'), 'the menu');
		const items = await page.run(`return [...document.querySelectorAll('.menu [role=menuitem]')].map((e) => e.textContent.trim());`);
		for (const want of ['Open', 'Show in folder', 'Rename…', 'Copy link', 'Remove…']) {
			if (!items.some((i) => i.startsWith(want))) throw new Error(`no "${want}" in ${items.join(', ')}`);
		}
		await page.key('Escape', 'Escape', 27);
		await waitFor(async () => !(await page.exists('.menu[role=menu]')), 'the menu to close');
	});

	await test('removing asks first; cancelled it keeps the download, confirmed it deletes the file too', async () => {
		const ask = async () => {
			await page.click(row, 'report.pdf', 'right');
			await waitFor(() => page.exists('.menu[role=menu]'), 'the menu');
			await page.click('.menu .danger');
			await waitFor(() => page.exists('dialog[open]', 'Remove this download from the list?'), 'the question');
		};
		await ask();
		await page.click('dialog[open] button.secondary', 'Cancel');
		await waitFor(async () => !(await page.exists('dialog[open]')), 'the question to close');
		if (!(await page.exists(row, 'report.pdf'))) throw new Error('removed anyway');
		await ask();
		await page.click('dialog[open] input[type=checkbox]');
		await page.click('dialog[open] button[type=submit]');
		await waitFor(async () => !(await page.exists(row, 'report.pdf')), 'the row to go');
		await waitFor(async () => !existsSync(downloaded), 'the file to go');
	});

	await test('a video from the browser: its qualities and sound, one chosen', async () => {
		const reply = fromBrowser(home, {
			type: 'media',
			page: `${SERVER}/watch`,
			title: 'Test clip',
			items: [{ url: VIDEO, kind: 'hls' }]
		});
		if (!reply.ok) throw new Error(JSON.stringify(reply));
		// The window asks the server for the stream's qualities once it is open.
		await waitFor(() => page.exists('dialog[open]', '720p'), 'the video window');
		const shown = await page.text('dialog[open]');
		for (const want of ['720p', '360p', 'Audio']) {
			if (!shown.includes(want)) throw new Error(`no ${want}: ${shown}`);
		}
		await page.click('dialog[open] label', '360p');
		await page.click('dialog[open] button.primary');
		await waitFor(async () => !(await page.exists('dialog[open]')), 'the window to close');
		await waitFor(() => page.exists(row, 'Test clip'), 'its download');
		// The quality chosen is the one downloaded. (Joining it into an MP4 needs real video,
		// which the test server does not make.)
		const link = await page.run(`return [...document.querySelectorAll('tbody tr')].find((r) => r.textContent.includes('Test clip')).querySelector('[title]')?.title ?? '';`);
		if (!link.includes('/hls/v360.m3u8')) throw new Error(`downloads ${link}`);
	});

	await test('a file from an FTP server, asked about and downloaded', async () => {
		const ftp = await ftpServer(join(home, 'served'));
		await page.click('aside button.primary');
		await waitFor(() => page.exists('dialog[open] input.field'), 'the dialog');
		await page.click('dialog[open] input.field');
		await page.type(`ftp://127.0.0.1:${ftp}/pub/data.iso`);
		await page.key('Tab', 'Tab', 9);
		await waitFor(() => page.exists('dialog[open] p.info', '5.00 MB'), 'the server’s answer');
		await page.click('dialog[open] button[type=submit]');
		await waitFor(() => page.exists('tbody tr.s-done', 'data.iso'), 'the download to finish', 60_000);
		execFileSync(exe('testserver'), ['verify', join(home, 'downloads', 'data.iso'), '--size', '5M', '--seed', '61']);
	});
}

/** An FTP server over `root` with `pub/data.iso` (5 MiB, seed 61) in it; its port. */
async function ftpServer(root) {
	const server = start(exe('testserver'), ['ftp', root, '--file', 'pub/data.iso:5M:61'], {
		stdio: ['ignore', 'pipe', 'ignore']
	});
	let out = '';
	server.stdout.on('data', (d) => (out += d));
	await waitFor(async () => /^ftp (\d+)/m.test(out), 'the FTP server');
	return Number(out.match(/^ftp (\d+)/m)[1]);
}

// ---- Main ---------------------------------------------------------------------------------

async function main() {
	if (process.platform !== 'win32') {
		console.log('the UI tests drive WebView2: Windows only');
		return 0;
	}
	for (const name of ['shardload', 'testserver', 'shardload-host']) {
		if (!existsSync(exe(name))) throw new Error(`build first: ${exe(name)} is missing`);
	}
	if (running('shardload.exe')) throw new Error('close Shardload first: the app lets one copy run');
	const home = HOME;
	rmSync(home, { recursive: true, force: true });
	mkdirSync(home, { recursive: true });
	const results = [];
	try {
		start(exe('testserver'), ['serve', '--bind', `127.0.0.1:${SERVER_PORT}`]);
		start(process.execPath, [join(APP, 'node_modules', 'vite', 'bin', 'vite.js'), 'dev', '--port', '1420', '--strictPort'], { cwd: APP });
		await waitFor(async () => (await fetch(`${SERVER}/_stats`)).ok, 'the test server');
		await waitFor(async () => (await fetch('http://localhost:1420/')).ok, 'Vite', 60_000);
		// What the app prints and logs is kept in ui-test/out/ to see why a run failed.
		mkdirSync(join(HERE, 'out'), { recursive: true });
		const output = openSync(join(HERE, 'out', 'app-output.txt'), 'w');
		start(exe('shardload'), [], {
			stdio: ['ignore', output, output],
			env: { ...process.env, SHARDLOAD_HOME: home, SHARDLOAD_DEVTOOLS_PORT: String(CDP_PORT) }
		});
		let target;
		let seen = 'nothing';
		await waitFor(async () => {
			try {
				const pages = await (await fetch(`http://127.0.0.1:${CDP_PORT}/json`)).json();
				seen = JSON.stringify(pages.map((p) => `${p.type} ${p.url}`));
				target = pages.find((p) => p.type === 'page' && p.url.startsWith('http://localhost:1420') && !p.url.includes('progress'));
			} catch (e) {
				seen = String(e.cause ?? e);
			}
			return !!target;
		}, 'the app’s window', 60_000).catch((e) => {
			throw new Error(`${e.message} (DevTools: ${seen})
${webviews()}`);
		});
		const page = await Page.connect(target.webSocketDebuggerUrl);
		await waitFor(() => page.exists('.app aside'), 'the list');
		await page.send('Emulation.setDeviceMetricsOverride', { width: 1100, height: 680, deviceScaleFactor: 1, mobile: false });
		await flows(page, home, async (name, body) => {
			try {
				await body();
				results.push(true);
				console.log(`ok    ${name}`);
			} catch (e) {
				results.push(false);
				failures++;
				console.log(`FAIL  ${name}: ${e.message}`);
			}
		});
		page.close();
	} finally {
		stopAll();
		await sleep(500);
		if (existsSync(join(home, 'logs'))) cpSync(join(home, 'logs'), join(HERE, 'out', 'logs'), { recursive: true });
		rmSync(home, { recursive: true, force: true, maxRetries: 5 });
	}
	const failed = results.filter((ok) => !ok).length;
	console.log(`${results.length - failed} passed, ${failed} failed`);
	return failed ? 1 : 0;
}

main().then(
	(code) => process.exit(code),
	(e) => {
		console.error(e.message);
		stopAll();
		process.exit(1);
	}
);
