#!/usr/bin/env node
// plugin-build — compile a plugin and pack it into a .spk.
//
//   plugin-build                        # build + unsigned package (local dev)
//   plugin-build --platform <triple>    # cross-pack for a device
//   plugin-build --key release.pem      # sign it (release path)
//
// Unsigned output installs only where svcd.conf has
// `allow_insecure_install = on`; CI signs with the release key instead.

import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  PLUGIN_BIN,
  assertPayload,
  buildNative,
  fail,
  findPluginRoot,
  hostTriple,
  packPackage,
  readManifest,
  run,
} from './lib/build.mjs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

function usage() {
  console.error(`usage: plugin-build [options]

  --dir <path>       plugin directory (default: walk up from cwd)
  --platform <t>     target triple (default: this host)
  --key <pem>        sign with this key; without it the package is unsigned
  --out <file>       write the .spk here
  --no-verify        skip the verify sanity check`);
}

function parse(argv) {
  const options = { dir: null, platform: null, key: null, out: null, verify: true };
  const takesValue = {
    '--dir': 'dir',
    '--platform': 'platform',
    '--key': 'key',
    '-k': 'key',
    '--out': 'out',
    '-o': 'out',
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--no-verify') options.verify = false;
    else if (arg === '-h' || arg === '--help') {
      usage();
      process.exit(0);
    } else if (takesValue[arg]) {
      const value = argv[index + 1];
      if (!value) fail(`${arg} needs a value`);
      options[takesValue[arg]] = value;
      index += 1;
    } else if (arg.startsWith('--') && arg.includes('=')) {
      const [flag, value] = arg.split(/=(.*)/s);
      if (!takesValue[flag]) fail(`unknown option ${flag}`);
      options[takesValue[flag]] = value;
    } else {
      fail(`unexpected argument ${arg}`);
    }
  }
  return options;
}

function main() {
  const options = parse(process.argv.slice(2));
  const root = findPluginRoot(options.dir ?? process.cwd());
  const manifest = readManifest(root);

  buildNative(root, manifest);
  assertPayload(root);

  const platform = options.platform ?? process.env.SVCD_TARGET ?? hostTriple();
  const spk = packPackage(root, manifest, {
    platform,
    key: options.key,
    out: options.out ? resolve(options.out) : null,
  });

  if (options.verify) {
    run(PLUGIN_BIN, ['verify', spk, '--json']);
  }

  console.log(`\npacked ${spk}${options.key ? '' : '  (unsigned)'}`);
  if (!options.key) {
    console.log('host needs: allow_insecure_install = on   (restart svcd after changing it)');
  }
  console.log(`next: plugin install "${spk}" && plugin start ${manifest.name}`);
}

main();
