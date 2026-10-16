import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
	DEFAULT_SETTINGS,
	baseName,
	cookieHeader,
	extension,
	isExcluded,
	parseList,
	shouldCapture,
	type Candidate
} from './filter.ts';

const zip: Candidate = { url: 'https://cdn.example.com/a/file.zip?x=1', filename: '', size: -1, ours: false };

test('captures listed types and leaves the rest', () => {
	assert.equal(shouldCapture(zip, DEFAULT_SETTINGS, false), true);
	assert.equal(shouldCapture({ ...zip, url: 'https://x.com/page.html' }, DEFAULT_SETTINGS, false), false);
	// The browser's chosen name wins over the URL.
	assert.equal(
		shouldCapture({ ...zip, url: 'https://x.com/get?id=1', filename: 'C:\\Users\\a\\Downloads\\Setup.EXE' }, DEFAULT_SETTINGS, false),
		true
	);
	// No extension anywhere: let the app ask the server.
	assert.equal(shouldCapture({ ...zip, url: 'https://x.com/download/123' }, DEFAULT_SETTINGS, false), true);
	// Every type when the list is empty.
	assert.equal(
		shouldCapture({ ...zip, url: 'https://x.com/a.html' }, { ...DEFAULT_SETTINGS, extensions: [] }, false),
		true
	);
});

test('never captures what the app cannot fetch, or when told not to', () => {
	assert.equal(shouldCapture({ ...zip, url: 'blob:https://x.com/uuid' }, DEFAULT_SETTINGS, false), false);
	assert.equal(shouldCapture({ ...zip, url: 'data:application/zip;base64,AA' }, DEFAULT_SETTINGS, false), false);
	assert.equal(shouldCapture({ ...zip, url: 'not a url' }, DEFAULT_SETTINGS, false), false);
	assert.equal(shouldCapture(zip, DEFAULT_SETTINGS, true), false, 'Alt held');
	assert.equal(shouldCapture({ ...zip, ours: true }, DEFAULT_SETTINGS, false), false, 'own fallback');
	assert.equal(shouldCapture(zip, { ...DEFAULT_SETTINGS, enabled: false }, false), false);
});

test('size and site filters', () => {
	const min = { ...DEFAULT_SETTINGS, minSizeKb: 500 };
	assert.equal(shouldCapture({ ...zip, size: 100 * 1024 }, min, false), false);
	assert.equal(shouldCapture({ ...zip, size: 600 * 1024 }, min, false), true);
	// Unknown size: captured, the app finds out.
	assert.equal(shouldCapture({ ...zip, size: -1 }, min, false), true);

	const ex = { ...DEFAULT_SETTINGS, excludedSites: ['example.com'] };
	assert.equal(shouldCapture(zip, ex, false), false, 'subdomain');
	assert.equal(shouldCapture({ ...zip, url: 'https://notexample.com/a.zip' }, ex, false), true);
	assert.equal(isExcluded('Example.COM', ['*.example.com']), true);
	assert.equal(isExcluded('a.b', ['']), false);
});

test('helpers', () => {
	assert.equal(baseName('/dir/%C3%A7ay%20bardak.zip'), 'çay bardak.zip');
	assert.equal(baseName('C:\\x\\y.pdf'), 'y.pdf');
	assert.equal(baseName('/%E0%A4%A'), '%E0%A4%A');
	assert.equal(extension('a.TAR.GZ'), 'gz');
	assert.equal(extension('.bashrc'), '');
	assert.equal(cookieHeader([]), null);
	assert.equal(cookieHeader([{ name: 'a', value: '1' }, { name: 'b', value: '2' }]), 'a=1; b=2');
	assert.deepEqual(parseList(' zip, .PDF  rar;zip\n'), ['zip', 'pdf', 'rar']);
});
