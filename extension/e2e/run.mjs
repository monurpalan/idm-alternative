// End-to-end tests of the Chromium extension in Chrome for Testing: a browser of its own (its
// own profile) with a copy of the built extension loaded, and a stand-in host (stub-host.mjs)
// that records what the extension sends. A local web server gives it things to download; the
// Chrome DevTools Protocol clicks.
//
// Chrome for Testing looks for native messaging hosts where Chrome does, so the copy talks to a
// host of another name (`...shardload.e2e`), registered for the test alone: an installed
// Shardload, which Chrome finds under the real name, is never reached, and nothing the user's
// browsers use is changed.
//
//   npm run build && npm run e2e
//
// Chrome for Testing is downloaded once into a cache folder (SHARDLOAD_E2E_CACHE, or
// %LOCALAPPDATA%\shardload-test / ~/.cache/shardload-test), or taken from
// SHARDLOAD_E2E_CHROME. SHARDLOAD_E2E_HEADED=1 shows the window.

import { spawn, execFileSync } from 'node:child_process';
import { chmodSync, cpSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { homedir, tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const BUILT = resolve(HERE, '..', 'dist', 'chromium');
const REAL_HOST = 'io.github.monurpalan.shardload';
const HOST = `${REAL_HOST}.e2e`;
const EXTENSION_ID = 'calgjlgiljgeegibddigkflnpegdhhle';
const WINDOWS = process.platform === 'win32';
const REG_KEY = `HKCU\\Software\\Google\\Chrome\\NativeMessagingHosts\\${HOST}`;
// Chrome for Testing's own folder and Chrome's: the test host is written to both.
const LINUX_HOSTS = ['google-chrome-for-testing', 'google-chrome'].map((b) =>
	join(homedir(), '.config', b, 'NativeMessagingHosts')
);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---- Chrome for Testing -------------------------------------------------------------------

async function chrome() {
	if (process.env.SHARDLOAD_E2E_CHROME) return process.env.SHARDLOAD_E2E_CHROME;
	const cache =
		process.env.SHARDLOAD_E2E_CACHE ??
		(WINDOWS ? join(process.env.LOCALAPPDATA, 'shardload-test') : join(homedir(), '.cache', 'shardload-test'));
	const platform = WINDOWS ? 'win64' : 'linux64';
	const index = await (
		await fetch('https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json')
	).json();
	const stable = index.channels.Stable;
	const dir = join(cache, `chrome-${stable.version}`);
	const exe = join(dir, `chrome-${platform}`, WINDOWS ? 'chrome.exe' : 'chrome');
	if (existsSync(exe)) return exe;
	const url = stable.downloads.chrome.find((d) => d.platform === platform).url;
	console.log(`downloading Chrome for Testing ${stable.version}`);
	mkdirSync(dir, { recursive: true });
	const zip = join(dir, 'chrome.zip');
	writeFileSync(zip, Buffer.from(await (await fetch(url)).arrayBuffer()));
	if (WINDOWS) {
		execFileSync('powershell', ['-NoProfile', '-Command', `Expand-Archive -Path '${zip}' -DestinationPath '${dir}' -Force`]);
	} else {
		execFileSync('unzip', ['-q', '-o', zip, '-d', dir]);
	}
	rmSync(zip);
	return exe;
}

// ---- The stand-in host --------------------------------------------------------------------

/** Registers the stub host for Chrome for Testing only; returns the function undoing it. */
function registerHost(work) {
	const script = join(HERE, 'stub-host.mjs');
	let launcher;
	if (WINDOWS) {
		launcher = join(work, 'stub-host.cmd');
		writeFileSync(launcher, `@echo off\r\n"${process.execPath}" "${script}" %*\r\n`);
	} else {
		launcher = join(work, 'stub-host.sh');
		writeFileSync(launcher, `#!/bin/sh\nexec "${process.execPath}" "${script}" "$@"\n`);
		chmodSync(launcher, 0o755);
	}
	const manifest = {
		name: HOST,
		description: 'Shardload end-to-end test host',
		path: launcher,
		type: 'stdio',
		allowed_origins: [`chrome-extension://${EXTENSION_ID}/`]
	};
	if (WINDOWS) {
		const file = join(work, 'host.json');
		writeFileSync(file, JSON.stringify(manifest));
		execFileSync('reg', ['add', REG_KEY, '/ve', '/t', 'REG_SZ', '/d', file, '/f'], { stdio: 'ignore' });
		return () => execFileSync('reg', ['delete', REG_KEY, '/f'], { stdio: 'ignore' });
	}
	const files = LINUX_HOSTS.map((dir) => {
		mkdirSync(dir, { recursive: true });
		const file = join(dir, `${HOST}.json`);
		writeFileSync(file, JSON.stringify(manifest));
		return file;
	});
	return () => files.forEach((f) => rmSync(f, { force: true }));
}

/** A copy of the built extension that talks to the test host. */
function testCopy(work) {
	const dir = join(work, 'extension');
	cpSync(BUILT, dir, { recursive: true });
	for (const name of readdirSync(dir).filter((n) => n.endsWith('.js'))) {
		const file = join(dir, name);
		const code = readFileSync(file, 'utf8');
		const copy = code.replaceAll(`'${REAL_HOST}'`, `'${HOST}'`).replaceAll(`"${REAL_HOST}"`, `"${HOST}"`);
		// Never let the test reach a real app.
		if (copy.replaceAll(HOST, '').includes(REAL_HOST)) throw new Error(`${name} names the real host`);
		writeFileSync(file, copy);
	}
	return dir;
}

/** The messages the extension sent so far. */
function sent(log) {
	if (!existsSync(log)) return [];
	return readFileSync(log, 'utf8')
		.split('\n')
		.filter(Boolean)
		.map((l) => JSON.parse(l));
}

// ---- The web server -----------------------------------------------------------------------

function serve() {
	const file = (res, name, type = 'application/zip') => {
		res.writeHead(200, {
			'content-type': type,
			'content-disposition': `attachment; filename="${name}"`,
			'content-length': 65536
		});
		res.end(Buffer.alloc(65536, 7));
	};
	const server = createServer((req, res) => {
		const url = new URL(req.url, 'http://x');
		if (url.pathname === '/') {
			res.writeHead(200, { 'content-type': 'text/html', 'set-cookie': 'sid=e2e; Path=/' });
			res.end(`<!doctype html><title>e2e</title><style>a,button{display:block;margin:8px;font-size:20px}</style>
<a id="plain" href="/files/plain.zip">plain</a>
<a id="refuse" href="/files/refuse.zip">refuse</a>
<a id="alt" href="/files/alt.zip">alt</a>
<form method="post" action="/export"><button id="post">post</button></form>
<form method="post" action="/redirect"><button id="postredirect">post then redirect</button></form>`);
		} else if (url.pathname.startsWith('/files/')) {
			file(res, url.pathname.slice('/files/'.length));
		} else if (url.pathname === '/export' && req.method === 'POST') {
			file(res, 'export.zip');
		} else if (url.pathname === '/redirect' && req.method === 'POST') {
			res.writeHead(303, { location: '/files/after-post.zip' });
			res.end();
		} else if (url.pathname === '/video.html') {
			res.writeHead(200, { 'content-type': 'text/html' });
			res.end(`<!doctype html><title>video</title><body style="margin:0">
<video id="v" src="/clip.mp4" width="640" height="360" muted preload="auto" style="display:block;background:#000"></video>`);
		} else if (url.pathname === '/clip.mp4') {
			res.writeHead(200, { 'content-type': 'video/mp4', 'content-length': 4096 });
			res.end(Buffer.alloc(4096));
		} else {
			res.writeHead(404);
			res.end();
		}
	});
	return new Promise((r) => server.listen(0, '127.0.0.1', () => r(server)));
}

// ---- The DevTools Protocol ----------------------------------------------------------------

class Cdp {
	#ws;
	#id = 0;
	#pending = new Map();
	#listeners = [];

	static async connect(url) {
		const cdp = new Cdp();
		cdp.#ws = new WebSocket(url);
		await new Promise((ok, fail) => {
			cdp.#ws.onopen = ok;
			cdp.#ws.onerror = fail;
		});
		cdp.#ws.onmessage = (e) => {
			const m = JSON.parse(e.data);
			if (m.id !== undefined) {
				const p = cdp.#pending.get(m.id);
				cdp.#pending.delete(m.id);
				if (m.error) p.fail(new Error(`${p.method}: ${m.error.message}`));
				else p.ok(m.result);
			} else {
				for (const l of cdp.#listeners) l(m);
			}
		};
		return cdp;
	}

	send(method, params = {}, sessionId) {
		const id = ++this.#id;
		this.#ws.send(JSON.stringify({ id, method, params, sessionId }));
		return new Promise((ok, fail) => this.#pending.set(id, { ok, fail, method }));
	}

	on(listener) {
		this.#listeners.push(listener);
	}

	close() {
		this.#ws.close();
	}
}

/** A tab at `url`, attached; returns its session id. */
async function open(cdp, url) {
	const { targetId } = await cdp.send('Target.createTarget', { url: 'about:blank' });
	const { sessionId } = await cdp.send('Target.attachToTarget', { targetId, flatten: true });
	await cdp.send('Page.enable', {}, sessionId);
	await cdp.send('Page.navigate', { url }, sessionId);
	await waitFor(async () => (await evaluate(cdp, sessionId, 'document.readyState')) === 'complete', 'page load');
	return sessionId;
}

async function evaluate(cdp, sessionId, expression) {
	const r = await cdp.send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true }, sessionId);
	return r.result.value;
}

/** The centre of the element `selector` in the page. */
async function centre(cdp, sessionId, selector) {
	const box = await evaluate(
		cdp,
		sessionId,
		`(() => { const r = document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect();
		  return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; })()`
	);
	return box;
}

/** Alt is 1 in the protocol's modifiers. */
async function click(cdp, sessionId, { x, y }, modifiers = 0) {
	await cdp.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y, modifiers }, sessionId);
	for (const type of ['mousePressed', 'mouseReleased']) {
		await cdp.send('Input.dispatchMouseEvent', { type, x, y, button: 'left', clickCount: 1, modifiers }, sessionId);
	}
}

async function waitFor(check, what, ms = 10_000) {
	const end = Date.now() + ms;
	while (Date.now() < end) {
		if (await check()) return;
		await sleep(100);
	}
	throw new Error(`timed out waiting for ${what}`);
}

// ---- The tests ----------------------------------------------------------------------------

async function main() {
	if (!existsSync(join(BUILT, 'manifest.json'))) throw new Error('build the extension first: npm run build');
	const exe = await chrome();
	const work = join(tmpdir(), `shardload-e2e-${process.pid}`);
	rmSync(work, { recursive: true, force: true });
	const profile = join(work, 'profile');
	const downloads = join(work, 'downloads');
	mkdirSync(profile, { recursive: true });
	mkdirSync(downloads, { recursive: true });
	const log = join(work, 'host.log');
	const EXTENSION = testCopy(work);
	const unregister = registerHost(work);
	const server = await serve();
	const base = `http://127.0.0.1:${server.address().port}`;
	const args = [
		`--user-data-dir=${profile}`,
		`--load-extension=${EXTENSION}`,
		`--disable-extensions-except=${EXTENSION}`,
		'--remote-debugging-port=0',
		'--no-first-run',
		'--no-default-browser-check',
		'--disable-search-engine-choice-screen',
		...(process.env.SHARDLOAD_E2E_HEADED ? [] : ['--headless=new']),
		...(WINDOWS ? [] : ['--no-sandbox']),
		'about:blank'
	];
	const browser = spawn(exe, args, {
		env: { ...process.env, SHARDLOAD_E2E_LOG: log },
		stdio: 'ignore'
	});
	const results = [];
	let cdp;
	try {
		const portFile = join(profile, 'DevToolsActivePort');
		await waitFor(() => existsSync(portFile) && readFileSync(portFile, 'utf8').includes('\n'), 'the browser', 30_000);
		const [port, path] = readFileSync(portFile, 'utf8').trim().split('\n');
		cdp = await Cdp.connect(`ws://127.0.0.1:${port}${path}`);
		await cdp.send('Browser.setDownloadBehavior', { behavior: 'allow', downloadPath: downloads, eventsEnabled: true });
		const finished = new Set();
		cdp.on((m) => {
			if (m.method === 'Browser.downloadProgress' && m.params.state === 'completed') finished.add(m.params.guid);
		});
		const onDisk = (name) => existsSync(join(downloads, name));
		const message = (pred) => sent(log).find(pred);

		async function test(name, body) {
			try {
				await body();
				results.push([name, true]);
				console.log(`ok    ${name}`);
			} catch (e) {
				results.push([name, false]);
				console.log(`FAIL  ${name}: ${e.message}`);
			}
		}

		await test('says hello to the app when it starts', async () => {
			await waitFor(() => message((m) => m.type === 'extension'), 'hello');
			const hello = message((m) => m.type === 'extension');
			if (!hello.version || !hello.browser) throw new Error(JSON.stringify(hello));
		});

		const page = await open(cdp, `${base}/`);

		await test('takes a download over, with its page and cookies', async () => {
			await click(cdp, page, await centre(cdp, page, '#plain'));
			await waitFor(() => message((m) => m.url?.endsWith('/files/plain.zip')), 'the download at the host');
			const m = message((m) => m.url?.endsWith('/files/plain.zip'));
			if (m.type !== 'download') throw new Error(`type ${m.type}`);
			if (m.referrer !== `${base}/`) throw new Error(`referrer ${m.referrer}`);
			if (!m.cookies?.includes('sid=e2e')) throw new Error(`cookies ${m.cookies}`);
			await sleep(1000);
			if (onDisk('plain.zip')) throw new Error('the browser downloaded it as well');
		});

		await test('gives a download the app refuses back to the browser', async () => {
			await click(cdp, page, await centre(cdp, page, '#refuse'));
			await waitFor(() => onDisk('refuse.zip'), 'the browser to download it');
			if (!message((m) => m.url?.endsWith('/files/refuse.zip'))) throw new Error('the host was not asked');
		});

		await test('Alt+click leaves the download to the browser', async () => {
			await cdp.send('Input.dispatchKeyEvent', { type: 'rawKeyDown', key: 'Alt', code: 'AltLeft', windowsVirtualKeyCode: 18, modifiers: 1 }, page);
			await click(cdp, page, await centre(cdp, page, '#alt'), 1);
			await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Alt', code: 'AltLeft', windowsVirtualKeyCode: 18 }, page);
			await waitFor(() => onDisk('alt.zip'), 'the browser to download it');
			if (message((m) => m.url?.endsWith('/files/alt.zip'))) throw new Error('the host got it');
		});

		await test('a form (POST) download stays in the browser', async () => {
			await sleep(1600); // past the Alt window
			await click(cdp, page, await centre(cdp, page, '#post'));
			await waitFor(() => onDisk('export.zip'), 'the browser to download it');
			if (message((m) => m.url?.includes('/export'))) throw new Error('the host got it');
		});

		const page2 = await open(cdp, `${base}/`);
		await test('a POST answered with a redirect to a plain address is taken over', async () => {
			await click(cdp, page2, await centre(cdp, page2, '#postredirect'));
			await waitFor(() => message((m) => m.url?.endsWith('/files/after-post.zip')), 'the download at the host');
			await sleep(1000);
			if (onDisk('after-post.zip')) throw new Error('the browser downloaded it as well');
		});

		await test('the button over a video sends it to the app', async () => {
			const video = await open(cdp, `${base}/video.html`);
			const at = await centre(cdp, video, '#v');
			let host = null;
			await waitFor(async () => {
				await cdp.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: at.x, y: at.y }, video);
				host = await evaluate(
					cdp,
					video,
					`(() => { const h = document.querySelector('shardload-video-button');
					  if (!h || getComputedStyle(h).display === 'none') return null;
					  const r = h.getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; })()`
				);
				return host !== null;
			}, 'the button');
			await click(cdp, video, host);
			await waitFor(() => message((m) => m.type === 'media'), 'the video at the host');
			const m = message((m) => m.type === 'media');
			if (!m.items?.some((i) => i.url === `${base}/clip.mp4`)) throw new Error(JSON.stringify(m.items));
			if (m.page !== `${base}/video.html`) throw new Error(`page ${m.page}`);
		});

		await test('the options page reaches the app and saves', async () => {
			const options = await open(cdp, `chrome-extension://${EXTENSION_ID}/options.html`);
			const status = () => evaluate(cdp, options, "document.getElementById('status').textContent");
			await waitFor(async () => (await status()).includes('Connected to Shardload e2e'), 'the status').catch(async (e) => {
				throw new Error(`${e.message}: ${await status()}`);
			});
			await evaluate(cdp, options, "document.getElementById('min').value = '100'; document.getElementById('form').requestSubmit(); true");
			await waitFor(async () => (await evaluate(cdp, options, "document.getElementById('saved').textContent")) === 'Saved', 'saving');
			await cdp.send('Page.reload', {}, options);
			await waitFor(async () => (await evaluate(cdp, options, "document.getElementById('min').value")) === '100', 'the saved value');
		});
	} finally {
		cdp?.close();
		browser.kill();
		server.close();
		unregister();
		await sleep(500);
		rmSync(work, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
	}
	const failed = results.filter(([, ok]) => !ok).length;
	console.log(`${results.length - failed} passed, ${failed} failed`);
	process.exit(failed ? 1 : 0);
}

main().catch((e) => {
	console.error(e);
	process.exit(1);
});
