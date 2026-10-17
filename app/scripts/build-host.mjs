// Builds the browser host and puts it where `tauri build` takes sidecars from
// (`src-tauri/binaries/idma-host-<target triple>.exe`); the installer places it next to the app.
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const run = (cmd, args) => execFileSync(cmd, args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] });

const triple = run('rustc', ['--print', 'host-tuple']).trim();
run('cargo', ['build', '--release', '-p', 'idma-native-host']);
const targetDir = process.env.CARGO_TARGET_DIR ?? join(root, 'target');
const out = join(root, 'app', 'src-tauri', 'binaries', `idma-host-${triple}.exe`);
mkdirSync(dirname(out), { recursive: true });
copyFileSync(join(targetDir, 'release', 'idma-host.exe'), out);
console.log(`browser host: ${out}`);
