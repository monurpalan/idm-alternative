// Keeps source files from growing past a size where they get hard to work in.
//
//   node scripts/check-size.mjs
//
// Code (.rs, .ts, .svelte, .mjs, .js) may have up to 600 lines; a Rust file's own tests (its
// `#[cfg(test)]` module at the end) and test files (`tests/`, `*.test.ts`, `e2e/`) up to 1000
// more. Translations, generated and built files are left out. A file that cannot be split yet
// is in size-allowlist.json with its size and why: it may not grow, and when it shrinks the
// list must say its new size (or lose the file once it is within the limit).

import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const CODE = 600;
const TESTS = 1000;
const EXTENSIONS = /\.(rs|ts|svelte|mjs|js)$/;
const SKIP = [/(^|\/)node_modules\//, /(^|\/)dist\//, /(^|\/)build\//, /\/i18n\/tr\//, /\.d\.ts$/, /(^|\/)gen\//];
const isTest = (f) => /(^|\/)(tests|e2e)\//.test(f) || /\.test\.ts$/.test(f);

const allow = JSON.parse(readFileSync(join(ROOT, 'size-allowlist.json'), 'utf8'));
const files = execFileSync('git', ['ls-files'], { cwd: ROOT, encoding: 'utf8' })
	.split('\n')
	.filter((f) => EXTENSIONS.test(f) && !SKIP.some((s) => s.test(f)));

/** Lines of code and lines of tests in `file`. */
function measure(file) {
	const lines = readFileSync(join(ROOT, file), 'utf8').replace(/\n$/, '').split('\n');
	if (isTest(file)) return { code: 0, tests: lines.length };
	const at = file.endsWith('.rs') ? lines.findIndex((l) => l === '#[cfg(test)]') : -1;
	return at < 0 ? { code: lines.length, tests: 0 } : { code: at, tests: lines.length - at };
}

const problems = [];
for (const file of files) {
	const { code, tests } = measure(file);
	const listed = allow[file];
	if (tests > TESTS)
		problems.push(`${file}: ${tests} lines of tests, more than ${TESTS}: move some into a test file of their own`);
	if (listed) {
		if (code > listed.lines)
			problems.push(`${file}: ${code} lines, more than the ${listed.lines} it is allowed (size-allowlist.json): it may not grow; split it`);
		else if (code <= CODE)
			problems.push(`${file}: ${code} lines, within ${CODE}: take it off size-allowlist.json`);
		else if (code < listed.lines)
			problems.push(`${file}: ${code} lines: lower its size in size-allowlist.json from ${listed.lines} to ${code}`);
	} else if (code > CODE) {
		problems.push(
			`${file}: ${code} lines of code, more than ${CODE}: split it by what its parts do (a module per part, ` +
				`as download/, hls/ and zip/ in the engine are)`
		);
	}
}
for (const file of Object.keys(allow)) {
	if (!files.includes(file)) problems.push(`size-allowlist.json names ${file}, which is gone: take it off`);
}

if (problems.length) {
	console.error(problems.join('\n'));
	process.exit(1);
}
console.log(`${files.length} files within ${CODE} lines (tests ${TESTS}), ${Object.keys(allow).length} allowed more`);
