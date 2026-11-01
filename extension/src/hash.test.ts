import { test } from 'node:test';
import assert from 'node:assert/strict';
import { asChecksum, hashesIn, hashForName, pick } from './hash.ts';

const S256 = 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855';
const S1 = 'da39a3ee5e6b4b0d3255bfef95601890afd80709';
const MD5 = 'd41d8cd98f00b204e9800998ecf8427e';
const S512 = 'cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e';

const one = (text: string) => {
	const h = pick(hashesIn(text));
	return h ? asChecksum(h) : null;
};

test('a labelled hash, any of the four', () => {
	assert.equal(one(`SHA-256: ${S256}`), `sha256:${S256}`);
	assert.equal(one(`sha256sum ${S256.toUpperCase()}`), `sha256:${S256}`);
	assert.equal(one(`SHA1 ${S1}`), `sha1:${S1}`);
	assert.equal(one(`MD5 checksum: ${MD5}`), `md5:${MD5}`);
	assert.equal(one(`SHA512 (file.iso) = ${S512}`), `sha512:${S512}`, 'BSD style: the length says it');
	assert.equal(one(`SHA-512: ${S512}`), `sha512:${S512}`);
});

test('unlabelled: only the lengths that cannot be anything else', () => {
	assert.equal(one(S256), `sha256:${S256}`);
	assert.equal(one(S512), `sha512:${S512}`);
	assert.equal(one(`commit ${S1}`), null, 'forty digits are as often a commit');
	assert.equal(one(`id ${MD5}`), null);
});

test('no false matches', () => {
	assert.equal(one(`SHA-1: ${S256}`), null, 'a label for another length');
	assert.equal(one('1234567890'.repeat(7).slice(0, 64)), null, 'all digits is a number');
	assert.equal(one(`x${S256}y`), null, 'part of a longer word');
	assert.equal(one(`${S256}0`), null, 'part of a longer hex');
	assert.equal(one('aaaa'.repeat(16)), null);
	assert.equal(one('no hash here'), null);
});

test('the strongest one; two that disagree are none', () => {
	assert.equal(one(`MD5: ${MD5} SHA-256: ${S256}`), `sha256:${S256}`);
	assert.equal(one(`SHA-1: ${S1} ${S256}`), `sha1:${S1}`, 'a labelled one before a guessed one');
	const other = S256.replace('e3', 'f4');
	assert.equal(one(`SHA-256: ${S256} SHA-256: ${other}`), null);
	assert.equal(one(`SHA-256: ${S256} again ${S256}`), `sha256:${S256}`, 'the same one twice is one');
});

test('a checksum list naming the file', () => {
	const sums = `${S256}  tool-1.2-linux.tar.gz\n${S256.replace('e3', '00')}  tool-1.2.zip\n${S256.replace('e3', '11')}  tool-1.2.zip.asc\n`;
	assert.equal(asChecksum(hashForName(sums, 'tool-1.2.zip')!), `sha256:${S256.replace('e3', '00')}`);
	assert.equal(hashForName(sums, 'tool.zip'), null);
	assert.equal(hashForName(sums, '1.2.zip'), null, 'a part of another name');
	const sha1 = `${S1} *setup.exe`;
	assert.equal(asChecksum(hashForName(sha1, 'setup.exe')!), `sha1:${S1}`, 'sha1sum output has no label');
	assert.equal(hashForName(sums, ''), null);
	assert.equal(asChecksum(hashForName(`SHA256 (tool-1.2.zip) = ${S256}`, 'tool-1.2.zip')!), `sha256:${S256}`);
	assert.equal(hashForName(`tool.zip built from commit ${S1}`, 'tool.zip'), null, 'a sentence is not a list');
});
