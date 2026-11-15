// Shared build/pack helpers for the plugin CLIs.
//
// `plugin-template` only lays a project down; `plugin-build` compiles it and
// packs the .spk. Both live here so the two commands cannot drift on where the
// binary goes or how the bundle is invoked.

import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

export const PLUGIN_BIN = process.env.SVCD_PLUGIN_BIN ?? 'plugin';

export function fail(message, code = 1) {
  console.error(`plugin: ${message}`);
  process.exit(code);
}

/** Run a command, inheriting stdio, and fail loudly on a non-zero exit. */
export function run(command, args, options = {}) {
  console.log(`$ ${command} ${args.join(' ')}`);
  const result = spawnSync(command, args, { stdio: 'inherit', ...options });
  if (result.error) fail(`${command}: ${result.error.message}`);
  if (result.status !== 0) fail(`${command} exited ${result.status}`);
}

/** Walk up from `start` until a `package/manifest.json` shows up. */
export function findPluginRoot(start) {
  let current = resolve(start);
  for (;;) {
    if (existsSync(join(current, 'package', 'manifest.json'))) return current;
    const parent = dirname(current);
    if (parent === current) break;
    current = parent;
  }
  fail('no package/manifest.json found; run inside a plugin directory or pass --dir');
}

export function readManifest(root) {
  return JSON.parse(readFileSync(join(root, 'package', 'manifest.json'), 'utf8'));
}

export function hostTriple() {
  const { platform, arch } = process;
  if (platform === 'win32' && arch === 'x64') return 'x86_64-pc-windows-msvc';
  if (platform === 'linux' && arch === 'x64') return 'x86_64-unknown-linux-musl';
  if (platform === 'linux' && arch === 'arm64') return 'aarch64-unknown-linux-musl';
  if (platform === 'darwin' && arch === 'arm64') return 'aarch64-apple-darwin';
  if (platform === 'darwin' && arch === 'x64') return 'x86_64-apple-darwin';
  fail(`cannot guess a target triple for ${platform}/${arch}; pass --platform`);
}

function countFiles(dir) {
  let count = 0;
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    count += entry.isDirectory() ? countFiles(join(dir, entry.name)) : 1;
  }
  return count;
}

/**
 * A package whose payload is empty is not worth panicking the packer over: the
 * usual cause is scaffolding without building yet.
 */
export function assertPayload(root) {
  const payload = join(root, 'package', 'payload');
  if (!existsSync(payload) || countFiles(payload) === 0) {
    fail('package/payload is empty: build first (native payload) or add the files the plugin ships');
  }
}

/**
 * Compile a native plugin and put the binary where `manifest.exec.path` points.
 * Script plugins have no Cargo.toml and skip this entirely.
 */
export function buildNative(root, manifest) {
  if (!existsSync(join(root, 'Cargo.toml'))) return;
  run('cargo', ['build', '--release'], { cwd: root });
  const binary = (manifest.exec?.path ?? '').split('/').pop();
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

/**
 * Pack `package/` into a .spk. Without a key this produces an unsigned package,
 * which installs only on a host with `allow_insecure_install`.
 */
export function packPackage(root, manifest, { platform, key = null, out = null }) {
  const spk =
    out ?? join(root, 'dist', `${manifest.name}-${manifest.version}-${platform}.spk`);
  mkdirSync(dirname(spk), { recursive: true });
  const args = ['bundle', join(root, 'package'), '--platform', platform, '-o', spk];
  if (key) args.push('--key', key);
  else args.push('--unsigned');
  run(PLUGIN_BIN, args);
  return spk;
}
