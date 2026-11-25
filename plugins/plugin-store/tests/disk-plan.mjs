// The store's space decision, checked against the real page source.
//
//   node plugins/plugin-store/tests/disk-plan.mjs
//
// `diskPlan` lives inside the page's module script, so this harness extracts it
// (plus `fmtBytes`) and runs it with a stubbed `fetch`. If the page is
// restructured and the markers move, the extraction throws instead of silently
// testing nothing.
//
// The rule each case pins: a fresh install proceeds when *any* root fits (the
// host picks the smallest that fits); an upgrade needs room for the old and
// new copy, so `free < new + old` but `free >= new` asks the operator, and
// `free < new` is a plain refusal.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const page = readFileSync(join(here, '..', 'package', 'payload', 'web', 'index.html'), 'utf8');
const body = page.split('<script type="module">')[1].split('</script>')[0];
const extract = (start, end) => {
  const from = body.indexOf(start);
  const to = body.indexOf(end, from);
  if (from < 0 || to < 0) throw new Error('page markers moved: ' + start);
  return body.slice(from, to);
};
const fmtBytes = /  function fmtBytes\(value\) \{[\s\S]*?\n  \}\n/.exec(body);
if (!fmtBytes) throw new Error('page markers moved: fmtBytes');
const diskPlanSource = extract('  async function diskPlan', '  // The dialog copy');

let asked = [];
let current = { roots: [] };
globalThis.fetch = async (url) => ({
  ok: true,
  json: async () => {
    const target = new URL('http://host' + url);
    if (target.pathname !== '/api/v1/disk') throw new Error('unexpected fetch: ' + url);
    return current;
  },
});

// `fmtBytes` is inlined as source, not passed as a value: `diskPlan` calls it
// directly, and passing the text would make every refusal path throw
// "fmtBytes is not a function" — which a swallow-all catch used to disguise as
// "too small".
const factory = new Function(
  'askToUninstall',
  fmtBytes[0].replace(/^  /gm, '') + '\n' + diskPlanSource + '\nreturn diskPlan;',
);
const diskPlan = factory(async (id, size, free) => {
  asked.push({ id, size, free });
  return { proceed: true, uninstall: id };
});

const root = (path, freeBytes, extra) => Object.assign({ root: path, freeBytes, default: path === '/data' }, extra || {});
const holder = (path, freeBytes, bytes) =>
  root(path, freeBytes, { plugin: { id: 'hysteria', version: '1.0.0', bytes } });

const cases = [
  ['fresh install, the only root fits', { roots: [root('/data', 20_000_000)] }, 'proceed'],
  ['fresh install, one of two roots fits', { roots: [root('/data', 4_000_000), root('/sd', 200_000_000)] }, 'proceed'],
  ['fresh install, nowhere fits', { roots: [root('/data', 4_000_000), root('/sd', 8_000_000)] }, 'too-small'],
  ['upgrade on the device: old 15.7 MiB, 17.5 MiB free', { roots: [holder('/data', 17_500_000, 15_700_000)] }, 'ask'],
  ['upgrade with room for both copies', { roots: [holder('/sd', 40_000_000, 15_700_000)] }, 'proceed'],
  ['upgrade where even the new copy does not fit', { roots: [holder('/data', 10_000_000, 15_700_000)] }, 'too-small'],
  ['a host without the route is not a blocker', null, 'proceed'],
];

const size = 15_700_000;
let failed = 0;
for (const [name, facts, want] of cases) {
  asked = [];
  current = facts;
  if (facts === null) {
    globalThis.fetch = async () => ({ ok: false, status: 404 });
  } else {
    globalThis.fetch = async () => ({ ok: true, json: async () => current });
  }
  let got;
  try {
    const plan = await diskPlan('hysteria', size);
    got = plan.uninstall ? 'ask' : 'proceed';
  } catch (error) {
    // Only the page's own refusal may be swallowed here. Any other throw is a
    // bug in the page and must fail the run, not masquerade as "too small".
    if (!String(error && error.message).includes('空间不够')) {
      throw new Error(name + ': unexpected failure: ' + (error && error.message ? error.message : error));
    }
    got = 'too-small';
  }
  let ok = got === want;
  if (!ok) {
    failed += 1;
    console.log('FAIL ' + name + ': wanted ' + want + ', got ' + got);
  }
  // The dialog is asked for exactly once, and only when the room is short.
  const askedWant = want === 'ask' ? 1 : 0;
  if (asked.length !== askedWant) {
    failed += 1;
    ok = false;
    console.log('FAIL ' + name + ': dialog asked ' + asked.length + ' time(s), wanted ' + askedWant);
  }
  if (want === 'ask' && (asked[0].id !== 'hysteria' || asked[0].size !== size)) {
    failed += 1;
    ok = false;
    console.log('FAIL ' + name + ': dialog named ' + JSON.stringify(asked[0]));
  }
  if (ok) console.log('ok   ' + name + ': ' + want);
}
process.exit(failed ? 1 : 0);
