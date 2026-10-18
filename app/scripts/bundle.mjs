// Builds the installer: the browser host and the MCP server as sidecars next to the app, and, when the update
// signing key is on this machine, the update signature and `latest.json` for GitHub Releases.
//
// The key is `%USERPROFILE%\.tauri\idma.key`; its password is in `idma.key.password` beside
// it, encrypted with Windows DPAPI (only this Windows user can read it). Without the key the
// installer is built unsigned and offers no update. `IDMA_SIGNING_KEY` names another key;
// further arguments go to `tauri build` (e.g. `-- --config test.json`).
import { execFileSync, execSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const app = join(dirname(fileURLToPath(import.meta.url)), '..');
const root = join(app, '..');
const run = (cmd, args) =>
	execFileSync(cmd, args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] });

// The browser host and the MCP server, where `tauri build` takes sidecars from.
const triple = run('rustc', ['--print', 'host-tuple']).trim();
run('cargo', ['build', '--release', '-p', 'idma-native-host', '-p', 'idma-mcp']);
const targetDir = process.env.CARGO_TARGET_DIR ?? join(root, 'target');
for (const bin of ['idma-host', 'idma-mcp']) {
	const sidecar = join(app, 'src-tauri', 'binaries', `${bin}-${triple}.exe`);
	mkdirSync(dirname(sidecar), { recursive: true });
	copyFileSync(join(targetDir, 'release', `${bin}.exe`), sidecar);
}

const config = JSON.parse(readFileSync(join(app, 'src-tauri', 'tauri.bundle.json'), 'utf8'));
const env = { ...process.env };
const key = process.env.IDMA_SIGNING_KEY ?? join(homedir(), '.tauri', 'idma.key');
const signing = existsSync(key);
if (signing) {
	config.bundle.createUpdaterArtifacts = true;
	env.TAURI_SIGNING_PRIVATE_KEY = readFileSync(key, 'utf8');
	const passwordFile = `${key}.password`;
	if (existsSync(passwordFile)) {
		env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD = execFileSync(
			'powershell',
			[
				'-NoProfile',
				'-Command',
				`$s = Get-Content -LiteralPath '${passwordFile}' | ConvertTo-SecureString; ` +
					'[Runtime.InteropServices.Marshal]::PtrToStringUni([Runtime.InteropServices.Marshal]::SecureStringToGlobalAllocUnicode($s))'
			],
			{ encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] }
		).trim();
	}
} else {
	console.log(`no signing key at ${key}: building without an update`);
}
const merged = join(tmpdir(), `idma-bundle-${process.pid}.json`);
writeFileSync(merged, JSON.stringify(config));
try {
	// npx is a .cmd on Windows, which only runs through a shell; quote every argument.
	const args = ['--config', merged, ...process.argv.slice(2)].map((a) => `"${a}"`).join(' ');
	execSync(`npx tauri build ${args}`, { cwd: app, env, stdio: 'inherit' });
} finally {
	rmSync(merged, { force: true });
}

if (signing) {
	const { version } = JSON.parse(readFileSync(join(app, 'src-tauri', 'tauri.conf.json'), 'utf8'));
	const nsis = join(targetDir, 'release', 'bundle', 'nsis');
	const setup = readdirSync(nsis).find((f) => f.endsWith(`_${version}_x64-setup.exe`));
	const signature = readFileSync(join(nsis, `${setup}.sig`), 'utf8');
	const latest = {
		version,
		pub_date: new Date().toISOString(),
		platforms: {
			'windows-x86_64': {
				signature,
				url: `https://github.com/monurpalan/idm-alternative/releases/download/v${version}/${setup}`
			}
		}
	};
	writeFileSync(join(nsis, 'latest.json'), JSON.stringify(latest, null, 2));
	console.log(`\nFor the release v${version}, upload these from ${nsis}:\n  ${setup}\n  latest.json`);
}
