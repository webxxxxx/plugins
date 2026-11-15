#!/usr/bin/env node
// plugin-template — scaffold a plugin, build it, and pack an unsigned .spk.
//
//   plugin-template wireguard --webui --lang rust
//
// Steps: instantiate templates/<lang> → vendor the shared SDK files → build →
// `plugin bundle --unsigned`. The result installs on a host with
// `allow_insecure_install = on`; a releasable package is built by CI with the
// release key instead (`plugin bundle --key …`).
//
// Exit codes: 0 ok, 1 usage/环境错误, 2 模板不存在.

import {
  chmodSync,
  cpSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const NAME_RE = /^[a-z0-9][a-z0-9_-]{0,63}$/;
const RESERVED = new Set(['svc', 'svcd', 'mcp', 'plugin', 'core', 'system', 'svcd-plugin']);
const TEXT_FILE = /\.(json|rs|toml|html|mjs|sh|md|txt)$/;
const PLUGIN_BIN = process.env.SVCD_PLUGIN_BIN ?? 'plugin';

function fail(message, code = 1) {
  console.error(`plugin-template: ${message}`);
  process.exit(code);
}

function usage() {
  console.error(`usage: plugin-template <name> --lang rust|shell [options]

  --webui            include package/payload/web (rust only)
  --desc <text>      one-line description for the manifest
  --platform <t>     target triple (default: this host)
  --dir <path>       where to create the plugin (default: plugins)
  --force            overwrite an existing directory
  --no-build         skip the build step
  --no-pack          skip the bundle step
  -k, --key <pem>    sign with this key instead of producing an unsigned package`);
}

function parseArgs(argv) {
  const options = {
    name: null,
    lang: null,
    webui: false,
    desc: null,
    platform: null,
    dir: 'plugins',
    force: false,
    build: true,
    pack: true,
    key: null,
  };
  const takesValue = {
    '--lang': 'lang',
    '--desc': 'desc',
    '--platform': 'platform',
    '--dir': 'dir',
    '--key': 'key',
    '-k': 'key',
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--webui') options.webui = true;
    else if (arg === '--force') options.force = true;
    else if (arg === '--no-build') options.build = false;
    else if (arg === '--no-pack') options.pack = false;
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
    } else if (arg.startsWith('-')) fail(`unknown option ${arg}`);
    else if (options.name === null) options.name = arg;
    else fail(`unexpected argument ${arg}`);
  }
  return options;
}

function pascal(name) {
  return name
    .split(/[-_]+/)
    .filter(Boolean)
    .map((part) => part[0].toUpperCase() + part.slice(1))
    .join('');
}

function render(text, vars) {
  return text
    .replaceAll('__NAME_PASCAL__', vars.pascal)
    .replaceAll('__NAME__', vars.name)
    .replaceAll('__DESC__', vars.desc);
}

function instantiate(from, to, vars) {
  mkdirSync(to, { recursive: true });
  for (const entry of readdirSync(from, { withFileTypes: true })) {
    const source = join(from, entry.name);
    const target = join(to, entry.name.replaceAll('__NAME__', vars.name));
    if (entry.isDirectory()) {
      instantiate(source, target, vars);
      continue;
    }
    if (TEXT_FILE.test(entry.name)) {
      writeFileSync(target, render(readFileSync(source, 'utf8'), vars));
    } else {
      cpSync(source, target);
    }
    if (entry.name.endsWith('.sh')) {
      try {
        chmodSync(target, 0o755);
      } catch {
        // Windows: the executable bit lives in git, not on the filesystem.
      }
    }
  }
}

function vendorSdk(pluginDir, lang) {
  const pkg = join(pluginDir, 'package');
  const web = join(pkg, 'payload', 'web');
  if (existsSync(web)) {
    cpSync(join(ROOT, 'sdk', 'web', 'plugin-ui.mjs'), join(web, 'plugin-ui.mjs'));
  }
  if (lang === 'shell') {
    const lib = join(pkg, 'payload', 'lib');
    mkdirSync(lib, { recursive: true });
    cpSync(join(ROOT, 'sdk', 'shell', 'svcd-plugin.sh'), join(lib, 'svcd-plugin.sh'));
  }
  // Never leave a placeholder here: the packer has no ignore rules, so
  // anything under package/ ships inside the .spk.
  mkdirSync(join(pkg, 'payload', 'bin'), { recursive: true });
}

function applyWebui(pluginDir, lang, enabled) {
  const web = join(pluginDir, 'package', 'payload', 'web');
  const manifestPath = join(pluginDir, 'package', 'manifest.json');
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  if (enabled) {
    if (!existsSync(web)) fail(`the ${lang} template has no web ui to include`);
    return;
  }
  rmSync(web, { recursive: true, force: true });
  if (manifest.ui) {
    delete manifest.ui;
    writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  }
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

function run(command, args, options = {}) {
  console.log(`$ ${command} ${args.join(' ')}`);
  const result = spawnSync(command, args, { stdio: 'inherit', ...options });
  if (result.error) fail(`${command}: ${result.error.message}`);
  if (result.status !== 0) fail(`${command} exited ${result.status}`);
}

function build(pluginDir, manifest) {
  if (!existsSync(join(pluginDir, 'Cargo.toml'))) return;
  run('cargo', ['build', '--release'], { cwd: pluginDir });
  const binary = (manifest.exec?.path ?? '').split('/').pop();
  if (!binary) fail('manifest.exec.path is missing');
  const candidates = [
    join(pluginDir, 'target', 'release', binary),
    join(pluginDir, 'target', 'release', `${binary}.exe`),
  ];
  const built = candidates.find((candidate) => existsSync(candidate));
  if (!built) fail(`cargo produced no ${binary} in target/release`);
  const target = join(pluginDir, 'package', 'payload', 'bin', binary);
  mkdirSync(dirname(target), { recursive: true });
  cpSync(built, target);
}

function countFiles(dir) {
  let count = 0;
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    count += entry.isDirectory() ? countFiles(join(dir, entry.name)) : 1;
  }
  return count;
}

function assertPayload(pluginDir) {
  const payload = join(pluginDir, 'package', 'payload');
  if (!existsSync(payload) || countFiles(payload) === 0) {
    fail('package/payload is empty: run without --no-build, or add the files the plugin ships');
  }
}

function pack(pluginDir, manifest, { platform, key }) {
  const out = join(pluginDir, 'dist', `${manifest.name}-${manifest.version}-${platform}.spk`);
  mkdirSync(dirname(out), { recursive: true });
  const args = ['bundle', join(pluginDir, 'package'), '--platform', platform, '-o', out];
  if (key) args.push('--key', key);
  else args.push('--unsigned');
  run(PLUGIN_BIN, args);
  return out;
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  if (!options.name) {
    usage();
    process.exit(1);
  }
  if (!options.lang) fail('--lang is required (rust or shell)');
  if (!NAME_RE.test(options.name)) {
    fail(`"${options.name}" is not a valid plugin name: [a-z0-9][a-z0-9_-]{0,63}, no dots`);
  }
  if (RESERVED.has(options.name)) fail(`"${options.name}" is reserved`);
  if (options.webui && options.lang !== 'rust') {
    fail(`--webui is not available for --lang ${options.lang} yet`);
  }

  const template = join(ROOT, 'templates', options.lang);
  if (!existsSync(template)) fail(`no template for --lang ${options.lang}`, 2);

  const pluginDir = resolve(ROOT, options.dir, options.name);
  if (existsSync(pluginDir) && !options.force) {
    fail(`${pluginDir} already exists (use --force to overwrite)`);
  }

  const vars = {
    name: options.name,
    pascal: pascal(options.name),
    desc: options.desc ?? `${options.name} plugin`,
  };
  instantiate(template, pluginDir, vars);
  vendorSdk(pluginDir, options.lang);
  applyWebui(pluginDir, options.lang, options.webui);

  const manifest = JSON.parse(readFileSync(join(pluginDir, 'package', 'manifest.json'), 'utf8'));

  if (options.build) build(pluginDir, manifest);
  if (!options.pack) {
    console.log(`created ${pluginDir} (build: ${options.build}, pack: skipped)`);
    return;
  }

  const platform = options.platform ?? process.env.SVCD_TARGET ?? hostTriple();
  assertPayload(pluginDir);
  const spk = pack(pluginDir, manifest, { platform, key: options.key });
  console.log(`\ncreated ${pluginDir}`);
  console.log(`packed  ${spk}${options.key ? '' : '  (unsigned — needs allow_insecure_install on the host)'}`);
  if (!options.key) {
    console.log(`install svcd.conf: allow_insecure_install = on   (then restart svcd)`);
  }
  console.log(`next: plugin install "${spk}" && plugin start ${manifest.name}`);
}

main();
