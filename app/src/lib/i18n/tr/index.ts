// Every Turkish text, keyed by the English one. Split by part of the interface so they can
// be edited apart; a key may appear in one file only (`npm run i18n` checks).
import add from './add.ts';
import common from './common.ts';
import detail from './detail.ts';
import main from './main.ts';
import settings from './settings.ts';

export const parts: Record<string, Record<string, string>> = { add, common, detail, main, settings };

export const tr: Record<string, string> = Object.assign({}, ...Object.values(parts));
