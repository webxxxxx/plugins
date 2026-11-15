// Shared build/pack helpers for the plugin CLIs.
//
// The driver knows no language. A template declares how to turn source into one
// file in `build.json` (a sibling of `package/`, never inside it), and this
// module just runs that command and copies the artifact it names. Adding a
// language is a template plus a recipe, not a change here.

import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
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

/** `build.json` beside `package/`, or null when the plugin ships no recipe. */
export function readRecipe(root) {
  const path = join(root, 'build.json');
  if (!existsSync(path)) return null;
  try {
    return JSON.parse(readFileSync(path, 'utf8'));
  } catch (error) {
    return fail(`build.json: ${error.message}`);
  }
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

function expand(value, vars) {
  return String(value)
    .replaceAll('{name}', vars.name)
    .replaceAll('{platform}', vars.platform)
    .replaceAll('{version}', vars.version);
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
 * Run the template's build recipe and put the artifact where
 * `manifest.exec.path` points.
 *
 * Recipe shape:
 *   { "build": {
 *       "command": ["cargo", "build", "--release"],
 *       "artifact": "target/release/{name}",
 *       "env": { "*": {"CGO_ENABLED": "0"}, "<triple>": {"GOOS": "linux"} }
 *   } }
 *
 * `build: null` (or a missing `build.json`) means the payload is already source
 * — script plugins need no compile step. Braces expand `{name}`, `{version}`
 * and `{platform}` so the same recipe works for every target.
 */
export function buildFromRecipe(root, manifest, recipe, platform) {
  const spec = recipe?.build;
  if (!spec) return;

  if (!Array.isArray(spec.command) || spec.command.length === 0) {
    fail('build.json: build.command must be a non-empty array');
  }
  if (typeof spec.artifact !== 'string' || spec.artifact.length === 0) {
    fail('build.json: build.artifact is required when build.command is set');
  }

  const vars = { name: manifest.name, version: manifest.version, platform };
  const command = spec.command.map((part) => expand(part, vars));
  const artifact = expand(spec.artifact, vars);

  // Create the artifact's directory first: most compilers refuse to create the
  // parent, and every language would otherwise repeat that in its command.
  mkdirSync(dirname(join(root, artifact)), { recursive: true });

  const env = { ...process.env };
  for (const layer of [spec.env?.['*'], spec.env?.[platform]]) {
    for (const [key, value] of Object.entries(layer ?? {})) {
      env[key] = expand(value, vars);
    }
  }

  run(command[0], command.slice(1), { cwd: root, env });

  const built = [join(root, artifact), join(root, `${artifact}.exe`)].find((candidate) =>
    existsSync(candidate),
  );
  if (!built) {
    fail(`build.json: no artifact at ${artifact} (or ${artifact}.exe) after the build`);
  }

  const execPath = manifest.exec?.path;
  if (!execPath) fail('manifest.exec.path is missing');

  // Keep the toolchain's suffix. On Windows the artifact is `<name>.exe`, and a
  // PE file renamed to `<name>` is invisible to CreateProcess (it only appends
  // `.exe`, it does not guess). The packaged manifest therefore needs the real
  // name, which is what this return value is for.
  const base = basename(execPath);
  const builtName = basename(built);
  const keepSuffix = !base.toLowerCase().endsWith('.exe') && builtName.toLowerCase().endsWith('.exe');
  const finalName = keepSuffix ? `${base}.exe` : base;

  const target = join(root, 'package', 'payload', 'bin', finalName);
  mkdirSync(dirname(target), { recursive: true });
  cpSync(built, target);
  return `payload/bin/${finalName}`;
}

/**
 * Pack `package/` into a .spk. Without a key this produces an unsigned package,
 * which installs only on a host with `allow_insecure_install`.
 *
 * `exec` overrides `manifest.exec.path` at pack time — used when a native build
 * produced a differently named artifact (see `buildFromRecipe`).
 */
export function packPackage(root, manifest, { platform, key = null, out = null, exec = null }) {
  const spk = out ?? join(root, 'dist', `${manifest.name}-${manifest.version}-${platform}.spk`);
  mkdirSync(dirname(spk), { recursive: true });
  const args = ['bundle', join(root, 'package'), '--platform', platform, '-o', spk];
  if (exec) args.push('--exec', exec);
  if (key) args.push('--key', key);
  else args.push('--unsigned');
  run(PLUGIN_BIN, args);
  return spk;
}
