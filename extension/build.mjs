// Builds the extension for Chromium browsers (Chrome, Edge, Brave, Opera, Vivaldi) into
// dist/chromium and for Firefox into dist/firefox. Same code; the manifests differ.

import { build } from 'esbuild';
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';

const pkg = JSON.parse(readFileSync('package.json', 'utf8'));
const { key } = JSON.parse(readFileSync('extension-key.json', 'utf8'));

const common = {
	manifest_version: 3,
	name: 'Shardload',
	version: pkg.version,
	description: 'Hands downloads to Shardload, a free and open-source IDM alternative.',
	permissions: ['downloads', 'nativeMessaging', 'cookies', 'contextMenus', 'scripting', 'storage', 'webRequest'],
	host_permissions: ['<all_urls>'],
	content_scripts: [
		{ matches: ['<all_urls>'], js: ['content.js'], run_at: 'document_start', all_frames: true },
		{ matches: ['<all_urls>'], js: ['video-button.js'], run_at: 'document_idle', all_frames: true }
	],
	options_ui: { page: 'options.html', open_in_tab: false },
	action: {
		default_title: 'Shardload',
		default_popup: 'popup.html',
		default_icon: { 16: 'icons/icon-16.png', 32: 'icons/icon-32.png' }
	},
	icons: { 16: 'icons/icon-16.png', 48: 'icons/icon-48.png', 128: 'icons/icon-128.png' }
};

const targets = {
	chromium: {
		...common,
		// Fixes the extension ID, which the native host's manifest names.
		key,
		background: { service_worker: 'background.js' }
	},
	firefox: {
		...common,
		background: { scripts: ['background.js'] },
		browser_specific_settings: {
			gecko: { id: 'idm-alternative@monurpalan.github.io', strict_min_version: '128.0' }
		}
	}
};

for (const [name, manifest] of Object.entries(targets)) {
	const out = `dist/${name}`;
	rmSync(out, { recursive: true, force: true });
	mkdirSync(out, { recursive: true });
	await build({
		entryPoints: ['src/background.ts', 'src/content.ts', 'src/video-button.ts', 'src/options.ts', 'src/popup.ts'],
		bundle: true,
		format: 'iife',
		target: 'es2022',
		outdir: out,
		logLevel: 'warning'
	});
	cpSync('src/options.html', `${out}/options.html`);
	cpSync('src/popup.html', `${out}/popup.html`);
	cpSync('src/ui.css', `${out}/ui.css`);
	cpSync('icons', `${out}/icons`, { recursive: true });
	writeFileSync(`${out}/manifest.json`, JSON.stringify(manifest, null, '\t') + '\n');
	console.log(`built ${out}`);
}
