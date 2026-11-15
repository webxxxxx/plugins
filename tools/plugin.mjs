#!/usr/bin/env node
// Local loop helpers that are not scaffolding:
//
//   node tools/plugin.mjs devkey    # once per machine, ~/.svcd-dev/dev.pem
//   node tools/plugin.mjs dev       # build + pack + install + start
//   node tools/plugin.mjs pack      # build + pack only
//   node tools/plugin.mjs check <file.spk>
//   node tools/plugin.mjs sync      # refresh the vendored SDK files
//
// Scaffolding is `plugin-template`; packing is `plugin-build`. This file keeps
// the install loop and the SDK-sync chore, and delegates the rest.

import { chmodSync, cpSync, existsSync, mkdirSync, readdirSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { generateKeyPairSync } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import {
  PLUGIN_BIN,
  assertPayload,
  buildFromRecipe,
  fail,
  findPluginRoot,
  hostTriple,
  packPackage,
  readManifest,
  readRecipe,
  run,
} from './lib/build.mjs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

function argValue(argv, flag) {
  const index = argv.indexOf(flag);
  return index >= 0 ? argv[index + 1] : null;
}

function devKey() {
  const dir = join(homedir(), '.svcd-dev');
  const key = join(dir, 'dev.pem');
  const pub = join(dir, 'dev.pub');
  if (existsSync(key)) {
    console.log(`dev key already there: ${key}`);
    return key;
  }
  mkdirSync(dir, { recursive: true });
  const pair = generateKeyPairSync('ed25519', {
    privateKeyEncoding: { type: 'pkcs8', format: 'pem' },
    publicKeyEncoding: { type: 'spki', format: 'pem' },
  });
  writeFileSync(key, pair.privateKey, { mode: 0o600 });
  writeFileSync(pub, pair.publicKey);
  try {
    chmodSync(key, 0o600);
  } catch {}
  console.log(`created ${key}\npublic: ${pub}`);
  return key;
}

function pack(argv) {
  const root = findPluginRoot(argValue(argv, '--dir') ?? process.cwd());
  const manifest = readManifest(root);
  const platform = argValue(argv, '--platform') ?? process.env.SVCD_TARGET ?? hostTriple();

  const exec = buildFromRecipe(root, manifest, readRecipe(root), platform);
  assertPayload(root);

  const key = argValue(argv, '--key') ?? null;
  const spk = packPackage(root, manifest, { platform, key, exec });
  console.log(`packed ${spk}${key ? '' : '  (unsigned)'}`);
  return spk;
}

function dev(argv) {
  const root = findPluginRoot(argValue(argv, '--dir') ?? process.cwd());
  const manifest = readManifest(root);
  const spk = pack(argv);
  run(PLUGIN_BIN, ['install', spk]);
  // First install does not auto-start; dev always wants it running.
  run(PLUGIN_BIN, ['start', manifest.name]);
  console.log(`started ${manifest.name}`);
}

function check(argv) {
  const file = argv.find((arg) => arg.endsWith('.spk'));
  if (!file) fail('usage: node tools/plugin.mjs check <file.spk>');
  run(PLUGIN_BIN, ['verify', resolve(file)]);
}

function sync() {
  const sdkWeb = join(ROOT, 'sdk', 'web', 'plugin-ui.mjs');
  const sdkShell = join(ROOT, 'sdk', 'shell', 'svcd-plugin.sh');
  let count = 0;
  for (const base of ['plugins', 'samples']) {
    const dir = join(ROOT, base);
    if (!existsSync(dir)) continue;
    for (const entry of readdirSync(dir)) {
      const pkg = join(dir, entry, 'package');
      if (!existsSync(join(pkg, 'manifest.json'))) continue;
      const web = join(pkg, 'payload', 'web');
      if (existsSync(web)) cpSync(sdkWeb, join(web, 'plugin-ui.mjs'));
      const lib = join(pkg, 'payload', 'lib');
      if (existsSync(lib)) cpSync(sdkShell, join(lib, 'svcd-plugin.sh'));
      count += 1;
    }
  }
  console.log(`synced ${count} plugin(s)`);
}

function main() {
  const argv = process.argv.slice(2);
  switch (argv[0]) {
    case 'devkey':
      devKey();
      break;
    case 'pack':
      pack(argv);
      break;
    case 'dev':
      dev(argv);
      break;
    case 'check':
      check(argv);
      break;
    case 'sync':
      sync();
      break;
    default:
      console.error('usage: node tools/plugin.mjs <devkey|pack|dev|check|sync> [options]');
      process.exit(1);
  }
}

main();
