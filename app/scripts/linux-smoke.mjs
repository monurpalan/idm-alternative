// Smoke test of an installed Linux package, on a (virtual) display: starts the app in a profile
// of its own, waits until its window's page has loaded and reached the app, checks the bridge
// socket the browser extension and the MCP server use, then downloads a file from the test
// server through the MCP server and checks it on disk.
//
//   xvfb-run -a node scripts/linux-smoke.mjs <app command> <mcp command> <testserver>
//
// e.g. `shardload shardload-mcp ../target/debug/testserver` after installing the .deb; an
// AppImage is run as `APPIMAGE_EXTRACT_AND_RUN=1 ./Shardload.AppImage`.

import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createInterface } from 'node:readline';

const [appCmd, mcpCmd, testserver] = process.argv.slice(2);
if (!appCmd || !mcpCmd || !testserver) {
	console.error('usage: linux-smoke.mjs <app> <mcp> <testserver>');
	process.exit(2);
}
const home = mkdtempSync(join(tmpdir(), 'shardload-smoke-'));
const env = { ...process.env, SHARDLOAD_HOME: home };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const children = [];

async function waitFor(check, what, ms) {
	const end = Date.now() + ms;
	while (Date.now() < end) {
		if (await check()) return;
		await sleep(250);
	}
	throw new Error(`timed out waiting for ${what}`);
}

function log() {
	const dir = join(home, 'logs');
	if (!existsSync(dir)) return '';
	return readdirSync(dir)
		.map((f) => readFileSync(join(dir, f), 'utf8'))
		.join('\n');
}

/** The MCP server over stdio: one JSON-RPC request at a time. */
function mcp() {
	const child = spawn(mcpCmd, [], { env, stdio: ['pipe', 'pipe', 'inherit'] });
	children.push(child);
	const lines = createInterface({ input: child.stdout });
	const waiting = new Map();
	lines.on('line', (line) => {
		const m = JSON.parse(line);
		waiting.get(m.id)?.(m);
		waiting.delete(m.id);
	});
	let id = 0;
	const request = (method, params) =>
		new Promise((resolve) => {
			waiting.set(++id, resolve);
			child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
		});
	return {
		request,
		notify: (method) => child.stdin.write(JSON.stringify({ jsonrpc: '2.0', method }) + '\n'),
		/** A tool's answer as JSON; throws when the tool failed. */
		async tool(name, args) {
			const r = await request('tools/call', { name, arguments: args });
			const text = r.result?.content?.[0]?.text ?? JSON.stringify(r.error);
			if (r.error || r.result?.isError) throw new Error(`${name}: ${text}`);
			return JSON.parse(text);
		}
	};
}

async function main() {
	const server = spawn(testserver, ['serve', '--bind', '127.0.0.1:8771'], { stdio: 'ignore' });
	children.push(server);
	// In a process group of its own, so stopping it stops the app too, not only the shell.
	const app = spawn('sh', ['-c', appCmd], { env, stdio: 'inherit', detached: true });
	children.push({ kill: () => process.kill(-app.pid) });
	app.on('exit', (code) => console.log(`the app exited (${code})`));

	await waitFor(() => log().includes("the window's page is ready"), "the window's page", 90_000);
	console.log("ok    the window's page loaded and reached the app");
	if (!existsSync(join(home, 'bridge.sock'))) throw new Error('no bridge socket');
	console.log('ok    the bridge socket is there');

	const client = mcp();
	await client.request('initialize', {
		protocolVersion: '2025-06-18',
		capabilities: {},
		clientInfo: { name: 'linux-smoke', version: '1' }
	});
	client.notify('notifications/initialized');
	const added = await client.tool('add_download', {
		url: 'http://127.0.0.1:8771/file/smoke.bin?size=4M&seed=5'
	});
	let task;
	await waitFor(
		async () => {
			const list = await client.tool('list_downloads', {});
			task = (list.downloads ?? list).find((t) => t.id === added.id);
			if (task?.status === 'error') throw new Error(`the download failed: ${task.error}`);
			return task?.status === 'done';
		},
		'the download',
		60_000
	);
	const size = statSync(task.path).size;
	if (size !== 4 << 20) throw new Error(`downloaded ${size} bytes`);
	console.log(`ok    downloaded through MCP: ${task.path}`);
}

main()
	.then(() => (process.exitCode = 0))
	.catch((e) => {
		console.error(`FAIL  ${e.message}`);
		console.error(log().split('\n').slice(-40).join('\n'));
		process.exitCode = 1;
	})
	.finally(async () => {
		for (const c of children.reverse()) {
			try {
				c.kill();
			} catch {
				// already gone
			}
		}
		await sleep(1000);
		rmSync(home, { recursive: true, force: true });
		process.exit(process.exitCode);
	});
