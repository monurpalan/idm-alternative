import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

// Set by `tauri dev` when developing on a device other than this machine.
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
	plugins: [
		sveltekit({
			// Tauri serves a single-page app: no server, every route falls back to index.html.
			adapter: adapter({ fallback: 'index.html' })
		})
	],
	// Keep Rust compiler output visible in `tauri dev`.
	clearScreen: false,
	server: {
		port: 1420,
		strictPort: true,
		host: host || false,
		hmr: host ? { protocol: 'ws', host, port: 1421 } : undefined,
		watch: { ignored: ['**/src-tauri/**'] }
	}
});
