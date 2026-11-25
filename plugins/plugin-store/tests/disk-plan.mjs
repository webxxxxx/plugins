// The store's pre-flight decision, checked against the real page source.
//
//   node plugins/plugin-store/tests/disk-plan.mjs
//
// `diskPlan`/`spacePlan`/`placePlan`/`rootRows` live inside the page's module
// script, so this harness extracts them and runs them with a stubbed `fetch`,
// `askToUninstall` and `askForRoot`. If the page is restructured and the markers
// move, the extraction throws instead of silently testing nothing.
//
// Two decisions, in order:
//
//   space (#19) — a fresh install proceeds when *any* root fits (the host picks
//   the smallest that fits); an upgrade needs room for the old and the new copy,
//   so `free < new + old` asks the operator. The dialog is worth asking only
//   while `free + old >= new`; anything below that is a plain refusal.
//
//   place (#299) — with `manualInstallRoot` on and more than one root the
//   operator picks the root, pre-selected with the one the host would have
//   taken: the recorded root for an installed plugin, else the smallest
//   measured root that fits. Off, a single root, or an older host without the
//   flag keeps the old behaviour: no dialog and no `root` in the request.
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
// The place decision and the dialog itself sit on opposite sides of this
// marker: `askForRoot` reaches for the DOM, so it stays outside the extracted
// region and is injected below.
const decisionSource = extract('  async function diskPlan', '  // The dialog copy');

let current = { roots: [] };
let pageHooks = { uninstall: [], roots: [] };
const rootChoice = new Map();
let rootRows;

const factory = new Function(
  'askToUninstall',
  'askForRoot',
  'rootChoice',
  fmtBytes[0].replace(/^  /gm, '') + '\n' + decisionSource + '\nreturn { diskPlan, rootRows };',
);
const { diskPlan, ...rest } = factory(
  async (id, size, free) => {
    pageHooks.uninstall.push({ id, size, free });
    return { proceed: true, uninstall: id };
  },
  async (id, roots, size) => {
    // What the picker's confirm button would send: the pre-selected row.
    const { rows, chosen } = rootRows(roots, id, size);
    pageHooks.roots.push({ id, size, chosen, rows });
    return chosen ? { root: chosen } : { cancelled: true };
  },
  rootChoice,
);
rootRows = rest.rootRows;

// `fetch` answers `/api/v1/disk`; everything else is a bug in the page.
globalThis.fetch = async (url) => {
  const target = new URL('http://host' + url);
  if (target.pathname !== '/api/v1/disk') throw new Error('unexpected fetch: ' + url);
  return { ok: true, json: async () => current };
};

const root = (path, freeBytes, extra) => Object.assign({ root: path, freeBytes, default: path === '/data', plugins: 0 }, extra || {});
const holder = (path, freeBytes, bytes) => root(path, freeBytes, { plugins: 1, plugin: { id: 'hysteria', version: '1.0.0', bytes } });
const on = (roots) => ({ manualInstallRoot: true, roots });
const off = (roots) => ({ manualInstallRoot: false, roots });

const size = 15_700_000;
const cases = [
  // --- space only: no switch in the answer, so the host picks the root ---
  ['fresh install, the only root fits', { roots: [root('/data', 20_000_000)] }, 'proceed', null],
  ['fresh install, one of two roots fits', { roots: [root('/data', 4_000_000), root('/sd', 200_000_000)] }, 'proceed', null],
  ['fresh install, nowhere fits', { roots: [root('/data', 4_000_000), root('/sd', 8_000_000)] }, 'too-small', null],
  ['upgrade on the device: old 15.7 MiB, 17.5 MiB free', { roots: [holder('/data', 17_500_000, 15_700_000)] }, 'ask', null],
  ['upgrade with room for both copies', { roots: [holder('/sd', 40_000_000, 15_700_000)] }, 'proceed', null],
  ['upgrade where only uninstalling makes room', { roots: [holder('/data', 10_000_000, 15_700_000)] }, 'ask', null],
  ['upgrade where even uninstalling does not free enough', { roots: [holder('/data', 1_000_000, 1_000_000)] }, 'too-small', null],
  ['a root the host cannot measure is not a blocker', { roots: [holder('/data', 0, 15_700_000)] }, 'proceed', null],
  ['a host without the route is not a blocker', null, 'proceed', null],

  // --- switch on/off (#299) ---
  ['switch off: two roots and the host picks', off([root('/data', 4_000_000), root('/sd', 200_000_000)]), 'proceed', null],
  ['switch on: one root has nothing to choose', on([root('/data', 200_000_000)]), 'proceed', null],
  ['switch on: the smallest root that fits is pre-selected', on([root('/data', 4_000_000), root('/sd', 200_000_000)]), 'proceed', '/sd'],
  ['switch on: the smaller of two fitting roots wins', on([root('/data', 20_000_000), root('/sd', 200_000_000)]), 'proceed', '/data'],
  ['switch on: an upgrade stays on the recorded root', on([holder('/sd', 200_000_000, 15_700_000), root('/data', 200_000_000)]), 'proceed', '/sd'],
  ['switch on: an unmounted root is shown, never chosen', on([root('/sd', null), root('/data', 200_000_000)]), 'proceed', '/data'],
  ['switch on: nowhere fits is still the space refusal', on([root('/data', 4_000_000), root('/sd', 8_000_000)]), 'too-small', null],
  ['switch on: an upgrade that must uninstall first asks no root', on([holder('/data', 10_000_000, 15_700_000)]), 'ask', null],
];

let failed = 0;
for (const [name, facts, want, wantRoot] of cases) {
  pageHooks = { uninstall: [], roots: [] };
  rootChoice.clear();
  current = facts;
  if (facts === null) {
    globalThis.fetch = async () => ({ ok: false, status: 404 });
  } else {
    globalThis.fetch = async () => ({ ok: true, json: async () => current });
  }
  let got;
  let plan = null;
  try {
    plan = await diskPlan('hysteria', size);
    got = plan.uninstall ? 'ask' : plan.cancelled ? 'cancelled' : 'proceed';
  } catch (error) {
    // Only the page's own refusal may be swallowed here. Any other throw is a
    // bug in the page and must fail the run, not masquerade as "too small".
    if (!String(error && error.message).includes('空间不够')) {
      throw new Error(name + ': unexpected failure: ' + (error && error.message ? error.message : error));
    }
    got = 'too-small';
  }
  const problems = [];
  if (got !== want) problems.push('wanted ' + want + ', got ' + got);
  if (plan && (plan.root || null) !== wantRoot) {
    problems.push('wanted root ' + wantRoot + ', got ' + (plan.root || null));
  }
  // The picker is asked exactly when the switch is on, there is more than one
  // root, and the space decision let the install through.
  const askedWant = want === 'proceed' && facts && facts.manualInstallRoot && facts.roots.length > 1 ? 1 : 0;
  if (pageHooks.roots.length !== askedWant) {
    problems.push('picker asked ' + pageHooks.roots.length + ' time(s), wanted ' + askedWant);
  }
  if (askedWant === 1 && pageHooks.roots.length === 1) {
    const asked = pageHooks.roots[0];
    if (asked.id !== 'hysteria' || asked.size !== size) {
      problems.push('picker named ' + JSON.stringify({ id: asked.id, size: asked.size }));
    }
    const selected = asked.rows.filter((row) => row.root === asked.chosen);
    if (selected.length !== 1) problems.push('the pre-selected root is not one row');
    // The space dialog belongs to the old decision and never fires here.
    if (pageHooks.uninstall.length !== 0) problems.push('both dialogs were asked');
  }
  if (problems.length === 0) {
    console.log('ok   ' + name + ': ' + want + (wantRoot ? ' → ' + wantRoot : ''));
    continue;
  }
  failed += 1;
  console.log('FAIL ' + name + ': ' + problems.join('; '));
}

// The rows are what the operator reads: a root that cannot take the package
// says why, and an installed plugin's other roots say they would be a second
// copy.
{
  const rows = rootRows([root('/data', 20_000_000), root('/sd', null), root('/tmp', 1_000_000)], '/data', size);
  const reasons = rows.rows.map((row) => row.reason);
  const wanted = ['', '卷没挂上，量不到空间', '放不下这个包'];
  if (JSON.stringify(reasons) === JSON.stringify(wanted) && rows.chosen === '/data') {
    console.log('ok   rootRows: fits, unmounted, too small, and /data is chosen');
  } else {
    failed += 1;
    console.log('FAIL rootRows: ' + JSON.stringify(rows));
  }
  const upgrade = rootRows([holder('/sd', 40_000_000, 1), root('/data', 90_000_000)], 'hysteria', size);
  if (upgrade.chosen === '/sd' && upgrade.rows[1].reason.includes('换根要先卸载')) {
    console.log('ok   rootRows: an upgrade stays on its recorded root');
  } else {
    failed += 1;
    console.log('FAIL rootRows (upgrade): ' + JSON.stringify(upgrade));
  }
}

// #299: one install run asks once. The shelf pre-check and the handoff after the
// download are the same decision, so the second call must reuse the answer
// instead of opening the picker again.
{
  pageHooks = { uninstall: [], roots: [] };
  rootChoice.clear();
  current = on([root('/data', 4_000_000), root('/sd', 200_000_000)]);
  const first = await diskPlan('hysteria', size);
  const second = await diskPlan('hysteria', size);
  if (pageHooks.roots.length === 1 && first.root === '/sd' && second.root === '/sd') {
    console.log('ok   the picker is asked once per install run');
  } else {
    failed += 1;
    console.log('FAIL picker asked ' + pageHooks.roots.length + ' time(s) for one install');
  }
}

// A cancelled picker cancels the install, and nothing is remembered for the
// next attempt.
{
  pageHooks = { uninstall: [], roots: [] };
  rootChoice.clear();
  current = on([root('/data', 4_000_000), root('/sd', 200_000_000)]);
  const cancelling = new Function(
    'askToUninstall',
    'askForRoot',
    'rootChoice',
    fmtBytes[0].replace(/^  /gm, '') + '\n' + decisionSource + '\nreturn diskPlan;',
  )(async () => ({ proceed: true }), async () => ({ cancelled: true }), rootChoice);
  const plan = await cancelling('hysteria', size);
  if (plan.cancelled === true && rootChoice.size === 0) {
    console.log('ok   a cancelled picker cancels the install and is not remembered');
  } else {
    failed += 1;
    console.log('FAIL cancel: ' + JSON.stringify(plan) + ' memo=' + rootChoice.size);
  }
}

// --- the picker itself (#299) ---
// No browser here, so the page's own DOM calls run against a minimal stand-in.
// What matters is what the dialog offers (rows, disabled reasons, the
// pre-selected one) and what it resolves with.
{
  const makeNode = (tag) => {
    const classes = new Set();
    const node = {
      tag,
      children: [],
      text: '',
      value: '',
      type: '',
      name: '',
      checked: false,
      disabled: false,
      onclick: null,
      onchange: null,
      append: (...kids) => node.children.push(...kids),
      classList: {
        add: (name) => classes.add(name),
        remove: (name) => classes.delete(name),
        toggle: (name, force) => (force ? classes.add(name) : classes.delete(name)),
        has: (name) => classes.has(name),
      },
    };
    Object.defineProperty(node, 'className', {
      get: () => [...classes].join(' '),
      set: (value) => {
        classes.clear();
        for (const name of String(value).split(/\s+/)) if (name) classes.add(name);
      },
    });
    Object.defineProperty(node, 'textContent', {
      get: () => node.text,
      set: (value) => {
        node.text = String(value);
        // Clearing the list is how the page empties it before rendering rows.
        if (node.text === '') node.children.length = 0;
      },
    });
    return node;
  };
  const rootList = makeNode('div');
  const rootModal = makeNode('div');
  rootModal.className = 'modal hidden';
  const rootCancel = makeNode('button');
  const rootConfirm = makeNode('button');
  const picker = new Function(
    'document',
    'rootList',
    'rootModal',
    'rootCancel',
    'rootConfirm',
    fmtBytes[0].replace(/^  /gm, '') + '\n' + decisionSource + '\n' +
      extract('  function describeRoot', '  async function downloadSpk') +
      '\nreturn askForRoot;',
  )({ createElement: (tag) => makeNode(tag) }, rootList, rootModal, rootCancel, rootConfirm);

  const problems = [];
  const radioOf = (row) => row.children[0];
  const textOf = (row) => row.children[1].children[0].textContent;
  const metaOf = (row) => row.children[1].children[1].textContent;

  // An upgrade: the recorded root is the only one offered, the others say why.
  const upgradeRows = [holder('/sd', 200_000_000, 15_700_000), root('/data', 200_000_000), root('/tmp', null)];
  const upgradeDialog = picker('hysteria', upgradeRows, size);
  const rows = rootList.children;
  if (rows.length !== 3) problems.push('3 rows, got ' + rows.length);
  if (!radioOf(rows[0]).checked || !rows[0].classList.has('chosen')) {
    problems.push('the recorded root must be pre-selected');
  }
  if (!radioOf(rows[1]).disabled || !rows[1].classList.has('off')) {
    problems.push('a second root of an installed plugin must be disabled');
  }
  if (!metaOf(rows[1]).includes('换根要先卸载')) problems.push('the second root must say why: ' + metaOf(rows[1]));
  if (!radioOf(rows[2]).disabled || !metaOf(rows[2]).includes('换根要先卸载')) {
    problems.push('every other root of an installed plugin must be disabled: ' + metaOf(rows[2]));
  }
  if (textOf(rows[0]) !== '/sd') problems.push('the row must name its root, got ' + textOf(rows[0]));
  if (!metaOf(rows[0]).includes('空闲 ') || !metaOf(rows[0]).includes('已装 1 个插件')) {
    problems.push('the row must show free space and the plugin count: ' + metaOf(rows[0]));
  }
  if (rootModal.classList.has('hidden')) problems.push('the dialog must be shown');
  if (rootConfirm.disabled) problems.push('confirm must be enabled when a root is usable');
  rootConfirm.onclick();
  const upgradeAnswer = await upgradeDialog;
  if (upgradeAnswer.root !== '/sd') problems.push('the upgrade must install on /sd, got ' + upgradeAnswer.root);
  if (!rootModal.classList.has('hidden')) problems.push('the dialog must close');

  // A fresh install: the smallest fitting root is pre-selected, and picking the
  // bigger one changes the answer that goes into the request.
  const freshRows = [
    root('/data', 20_000_000),
    root('/sd', 200_000_000),
    root('/tmp', null),
    root('/tiny', 1_000_000),
  ];
  const freshDialog = picker('hysteria', freshRows, size);
  const fresh = rootList.children;
  if (!radioOf(fresh[0]).checked) problems.push('the smallest fitting root must be pre-selected');
  if (textOf(fresh[0]) !== '/data（默认）') problems.push('the default root must be marked: ' + textOf(fresh[0]));
  if (!radioOf(fresh[2]).disabled || !metaOf(fresh[2]).includes('卷没挂上')) {
    problems.push('an unmounted root must be shown and disabled: ' + metaOf(fresh[2]));
  }
  if (!radioOf(fresh[3]).disabled || !metaOf(fresh[3]).includes('放不下这个包')) {
    problems.push('a root too small for the package must say so: ' + metaOf(fresh[3]));
  }
  radioOf(fresh[1]).checked = true;
  radioOf(fresh[1]).onchange();
  if (!fresh[1].classList.has('chosen') || fresh[0].classList.has('chosen')) {
    problems.push('choosing a row must move the highlight');
  }
  rootConfirm.onclick();
  const freshAnswer = await freshDialog;
  if (freshAnswer.root !== '/sd') problems.push('confirm must return the picked root, got ' + freshAnswer.root);

  // Cancel resolves without a root, and the dialog is hidden again.
  const cancelledDialog = picker('hysteria', freshRows, size);
  rootCancel.onclick();
  const cancelAnswer = await cancelledDialog;
  if (cancelAnswer.cancelled !== true || cancelAnswer.root) {
    problems.push('cancel must resolve { cancelled: true }: ' + JSON.stringify(cancelAnswer));
  }
  if (!rootModal.classList.has('hidden')) problems.push('cancel must close the dialog');

  if (problems.length === 0) {
    console.log('ok   the picker: rows, reasons, pre-selection, confirm and cancel');
  } else {
    failed += 1;
    for (const problem of problems) console.log('FAIL picker: ' + problem);
  }
}

// #19: the space decision only means something after the host's blob is gone,
// and no failure before the POST may leave that blob behind. Both are ordering
// facts in the page source, so pin them here instead of trusting a review.
const handoff = extract('  async function handoff(job)', '  // The host serves this page itself');
const start = extract('  async function startInstall(entry)', '  function updateJob(job)');
const order = (source, first, second, what) => {
  const a = source.indexOf(first);
  const b = source.indexOf(second);
  if (a < 0 || b < 0 || a > b) {
    failed += 1;
    console.log('FAIL ' + what);
    return false;
  }
  return true;
};
order(handoff, 'await releaseBlob(blobId);', 'await diskPlan(', 'the host blob must be released before the space check');
order(start, 'await diskPlan(', "await call('install'", 'the space check must run before the download starts');
if (!/finally \{[\s\S]*releaseBlob\(blobId\)/.test(handoff)) {
  failed += 1;
  console.log('FAIL every failure before the POST must release the blob in a finally block');
}
if (!/releaseBlob\(blobId\);\s*\n\s*blobId = null;/.test(handoff)) {
  failed += 1;
  console.log('FAIL the released blob must be forgotten so the finally does not delete twice');
}
// #299: the POST must carry the picked root, and the per-install answer must
// not outlive the attempt.
if (!/strategy=replace'[\s\S]{0,160}plan\.root \? '&root=' \+ encodeURIComponent\(plan\.root\)/.test(handoff)) {
  failed += 1;
  console.log('FAIL the install request must carry the root the operator picked');
}
if (!/rootChoice\.delete\(job\.name\)/.test(handoff)) {
  failed += 1;
  console.log('FAIL the picked root must be forgotten when the attempt ends');
}

process.exit(failed ? 1 : 0);
