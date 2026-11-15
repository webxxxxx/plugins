#!/usr/bin/env node
// plugin-template — lay a plugin project down from a template.
//
//   plugin-template wireguard --webui --lang rust
//
// This only creates files. Building and packing are `plugin-build`'s job, so a
// template run never depends on a toolchain being present.

import { cpSync, chmodSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const NAME_RE = /^[a-z0-9][a-z0-9_-]{0,63}$/;
const RESERVED = new Set(['svc', 'svcd', 'mcp', 'plugin', 'core', 'system', 'svcd-plugin']);
const TEXT_FILE = /\.(json|rs|toml|html|mjs|sh|md|txt)$/;
// A template directory is also a working tree: an editor's build output must not
// travel into the project. Skipped locally, not just via .gitignore.
const SKIP_DIRS = new Set(['target', 'dist', 'node_modules', '.git', '.idea', '.vscode']);

function fail(message, code = 1) {
  console.error(`plugin-template: ${message}`);
  process.exit(code);
}

function usage() {
  console.error(`usage: plugin-template <name> --lang rust|shell [options]

  --webui            include package/payload/web (a browser UI over the capabilities)
  --desc <text>      one-line description for the manifest
  --dir <path>       where to create the plugin (default: plugins)
  --force            overwrite an existing directory`);
}

function parseArgs(argv) {
  const options = { name: null, lang: null, webui: false, desc: null, dir: 'plugins', force: false };
  const takesValue = { '--lang': 'lang', '--desc': 'desc', '--dir': 'dir' };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--webui') options.webui = true;
    else if (arg === '--force') options.force = true;
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
    if (entry.isDirectory() && SKIP_DIRS.has(entry.name)) continue;
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

/**
 * `--webui` is opt-in. Dropping the UI has to remove both halves: the files and
 * the manifest key, otherwise the package declares a UI it does not carry.
 */
function applyWebui(pluginDir, enabled) {
  const web = join(pluginDir, 'package', 'payload', 'web');
  const manifestPath = join(pluginDir, 'package', 'manifest.json');
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));

  if (enabled) {
    if (!existsSync(web)) fail('this template has no web ui to include');
    if (!manifest.ui) {
      manifest.ui = { type: 'web', entry: 'index.html' };
      writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
    }
    return;
  }

  rmSync(web, { recursive: true, force: true });
  if (manifest.ui) {
    delete manifest.ui;
    writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  }
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

  const template = join(ROOT, 'templates', options.lang);
  if (!existsSync(template)) fail(`no template for --lang ${options.lang}`, 2);

  const pluginDir = resolve(ROOT, options.dir, options.name);
  if (existsSync(pluginDir) && !options.force) {
    fail(`${pluginDir} already exists (use --force to overwrite)`);
  }

  instantiate(template, pluginDir, {
    name: options.name,
    pascal: pascal(options.name),
    desc: options.desc ?? `${options.name} plugin`,
  });
  vendorSdk(pluginDir, options.lang);
  applyWebui(pluginDir, options.webui);

  console.log(`created ${pluginDir}${options.webui ? ' (with web ui)' : ''}`);
  console.log(`next: cd "${pluginDir}" && plugin-build`);
}

main();
