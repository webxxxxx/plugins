// Which variant a store row offers, checked against the real page source.
//
//   node plugins/plugin-store/tests/variants.mjs
//
// One row is one install slot: a plugin at one version on one platform. A
// device-restricted build (`<id>-<version>-<platform>-<device>.spk`) shares its
// id, version and slot with the portable package, so the host can hold exactly
// one of them — showing them as two rows made both say "upgrade" and neither
// say which one was installed (#350). `catalog::select` folds them into one row
// with `variants`; the page decides which variant to offer, in the order
// `installed` > operator's pick > this machine's device > portable.
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

// `pickVariant` has no DOM or state behind it: it is the preference order, and
// that is the whole point of pinning it here.
const source = extract('  function variantsOf(row)', '  function renderRow(row)');
const { pickVariant } = new Function(source + '\nreturn { variantsOf, pickVariant };')();

let failures = 0;
function check(name, condition, detail) {
  if (condition) {
    console.log('ok   ' + name);
  } else {
    failures += 1;
    console.log('FAIL ' + name + (detail === undefined ? '' : ': ' + JSON.stringify(detail)));
  }
}

const entry = (device) => ({
  name: 'wireguard',
  version: '1.2.4',
  platform: 'aarch64-unknown-linux-musl',
  device: device || null,
});
const universal = entry('');
const kernel = entry('sr6110');
const row = { name: 'wireguard', version: '1.2.4', platform: 'aarch64-unknown-linux-musl', variants: [universal, kernel] };
const choose = (preferences) => pickVariant(row, preferences);

check('an installed device build is the row default', choose(['sr6110', null, null, '']) === kernel);
check(
  'an installed portable build wins over the device build',
  choose(['', null, 'sr6110', '']) === universal,
);
check("this machine's device is the default when nothing is installed", choose([null, null, 'sr6110', '']) === kernel);
check('a portable row stays portable', choose([null, null, null, '']) === universal);
check('an unknown device falls back to portable', choose([null, null, 'other', '']) === universal);
check("the operator's pick outranks this machine's device", choose([null, 'sr6110', null, '']) === kernel);
check(
  'a pick that no longer exists is ignored instead of blank',
  choose([null, 'sr1010', 'sr6110', '']) === kernel,
);

// A single-artifact plugin keeps working: the server sends one variant, and the
// preference order simply has nothing else to pick.
const solo = { name: 'meow', version: '1.3.12', platform: 'aarch64-unknown-linux-musl', variants: [entry('')] };
check('a single variant row answers with that variant', pickVariant(solo, [null, null, 'sr6110', '']) === solo.variants[0]);
// A device-only artifact (nothing portable) still resolves.
const onlyKernel = { name: 'wireguard', version: '1.2.4', platform: 'aarch64-unknown-linux-musl', variants: [kernel] };
check('a device-only artifact is still reachable', pickVariant(onlyKernel, [null, null, null, '']) === kernel);
// The old wire shape (one artifact per row, no `variants`) must not throw.
check('a row without variants answers with itself', pickVariant({ name: 'old' }, [null, null, null, '']) !== undefined);

// The page has to actually use the order above; a row that renders `variants[0]`
// directly would show the portable package to a machine that installed the
// device build.
check(
  'renderRow asks pickVariant with the documented preference order',
  /pickVariant\(row, \[installedDevice, remembered, HOST\.device, ''\]\)/.test(body),
);
check(
  'the installed variant comes from the host, not from the catalog',
  /installedDevices\.get\(row\.name\)/.test(body) && /Array\.isArray\(service\.devices\)/.test(body),
);

if (failures === 0) console.log('\nall checks passed');
process.exit(failures ? 1 : 0);
