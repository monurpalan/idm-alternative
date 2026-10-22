import assert from 'node:assert/strict';
import { test } from 'node:test';
import { HELLO_EVERY_MS, PROTOCOL, RETRY_MS, browserName, helloDue, helloMessage, hostMissing } from './hello.ts';

const chromeUa =
	'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36';
const hints = { firefox: false, brands: [] as string[], brave: false, userAgent: chromeUa };

test('names the browser', () => {
	assert.equal(browserName({ ...hints, brands: ['Chromium', 'Google Chrome', 'Not?A_Brand'] }), 'chrome');
	assert.equal(browserName({ ...hints, brands: ['Chromium', 'Microsoft Edge'] }), 'edge');
	assert.equal(browserName({ ...hints, userAgent: `${chromeUa} Edg/141.0.0.0` }), 'edge');
	assert.equal(browserName({ ...hints, userAgent: `${chromeUa} OPR/120.0.0.0` }), 'opera');
	assert.equal(browserName({ ...hints, brands: ['Chromium', 'Brave'], brave: true }), 'brave');
	assert.equal(browserName({ ...hints, brands: ['Chromium'] }), 'chromium');
	assert.equal(browserName({ ...hints, firefox: true }), 'firefox');
});

test('says hello once an hour, and retries a failed one after a few minutes', () => {
	const now = 1_000_000_000;
	assert.equal(helloDue({}, now), true);
	assert.equal(helloDue({ ok: now - 1000, tried: now - 1000 }, now), false);
	assert.equal(helloDue({ ok: now - HELLO_EVERY_MS, tried: now - HELLO_EVERY_MS }, now), true);
	assert.equal(helloDue({ tried: now - 1000 }, now), false);
	assert.equal(helloDue({ tried: now - RETRY_MS }, now), true);
});

test('the hello carries the protocol', () => {
	assert.deepEqual(helloMessage('0.1.1', 'edge'), {
		type: 'extension',
		version: '0.1.1',
		browser: 'edge',
		protocol: PROTOCOL
	});
});

test('tells a missing app from one that failed', () => {
	assert.equal(hostMissing('Error: Specified native messaging host not found.'), true);
	assert.equal(hostMissing('Error: No such native application io.github.monurpalan.shardload'), true);
	assert.equal(hostMissing('Error: Native host has exited.'), false);
});
