#!/usr/bin/env node
// Local loop for a plugin: build, bundle, install, verify.
//
//   node tools/plugin.mjs devkey                 # once per machine
//   node tools/plugin.mjs dev                    # pack + install + start
//   node tools/plugin.mjs pack [--platform <t>]  # bundle only
//   node tools/plugin.mjs check <file.spk>       # `plugin verify`
//   node tools/plugin.mjs sync                   # refresh vendored SDK files
//
// Run from a plugin directory or anywhere inside the repo with --dir.

import { chmodSync, cpSync, existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve, sep } from 'node:path';
import { generateKeyPairSync } from 'node:crypto';
import { spawnSync } from 'node:child_process';

const ROOT = resolve(dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1')), '..');
const PLUGIN_BIN = process.env.SVCD_PLUGIN_BIN ?? 'plugin';

function fail(message) {
  console.error(`plugin: ${message}`);
  process.exit(1);
}

function argValue(argv, flag) {
  const index = argv.indexOf(flag);
  return index >= 0 ? argv[index + 1] : null;
}

function pluginRoot(argv) {
  const explicit = argValue(argv, '--dir');
  if (explicit) return resolve(explicit);
  let current = process.cwd();
  for (;;) {
    if (existsSync(join(current, 'package', 'manifest.json'))) return current;
    const parent = dirname(current);
    if (parent === current) break;
    current = parent;
  }
  fail('no package/manifest.json found; run inside a plugin directory or pass --dir');
}

function manifestOf(root) {
  return JSON.parse(readFileSync(join(root, 'package', 'manifest.json'), 'utf8'));
}

function hostTriple() {
  const { platform, arch } = process;
  if (platform === 'win32' && arch === 'x64') return 'x86_64-pc-windows-msvc';
  if (platform === 'linux' && arch === 'x64') return 'x86_64-unknown-linux-musl';
  if (platform === 'linux' && arch === 'arm64') return 'aarch64-unknown-linux-musl';
  if (platform === 'darwin' && arch === 'arm64') return 'aarch64-apple-darwin';
  if (platform === 'darwin' && arch === 'x64') return 'x86_64-apple-darwin';
  fail(`cannot guess a target triple for ${platform}/${arch}; pass --platform`);
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

function run(command, args, options = {}) {
  console.log(`$ ${command} ${args.join(' ')}`);
  const result = spawnSync(command, args, { stdio: 'inherit', ...options });
  if (result.error) fail(`${command}: ${result.error.message}`);
  if (result.status !== 0) fail(`${command} exited ${result.status}`);
}

function build(root, manifest) {
  if (!existsSync(join(root, 'Cargo.toml'))) return;
  run('cargo', ['build', '--release'], { cwd: root });
  const execPath = manifest.exec?.path ?? '';
  const binary = execPath.split('/').pop();
  if (!binary) fail('manifest.exec.path is missing');
  const candidates = [
    join(root, 'target', 'release', binary),
    join(root, 'target', 'release', `${binary}.exe`),
  ];
  const built = candidates.find((candidate) => existsSync(candidate));
  if (!built) fail(`cargo produced no ${binary} in target/release`);
  const target = join(root, 'package', 'payload', 'bin', binary);
  mkdirSync(dirname(target), { recursive: true });
  cpSync(built, target);
}

function pack(argv) {
  const root = pluginRoot(argv);
  const manifest = manifestOf(root);
  build(root, manifest);

  const platform = argValue(argv, '--platform') ?? process.env.SVCD_TARGET ?? hostTriple();
  const key = argValue(argv, '--key') ?? process.env.SVCD_DEV_KEY ?? devKey();
  const out = argValue(argv, '--out') ?? join(root, 'dist', `${manifest.name}-${manifest.version}-${platform}.spk`);
  mkdirSync(dirname(out), { recursive: true });

  run(PLUGIN_BIN, [
    'bundle',
    join(root, 'package'),
    '--platform',
    platform,
    '--key',
    key,
    '-o',
    out,
  ]);
  console.log(`packed ${out}`);
  return out;
}

function dev(argv) {
  const root = pluginRoot(argv);
  const manifest = manifestOf(root);
  const spk = pack(argv);
  if (!existsSync(join(root, '..', '..', 'tools'))) {
    // fall through; the repo layout is not load-bearing here
  }
  run(PLUGIN_BIN, ['install', spk]);
  // First install does not auto-start; dev always wants it running.
  run(PLUGIN_BIN, ['start', manifest.name]);
  const log = join(root, '..', '..');
  void log;
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
  const command = argv[0];
  switch (command) {
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
