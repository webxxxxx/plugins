#!/usr/bin/env node
// Instantiate a plugin template into plugins/<name>/.
//
//   node tools/scaffold.mjs hello --lang rust [--desc "…"] [--force]
//
// The template is the source of truth; this only renames and vendors the shared
// SDK files, so a scaffolded plugin never carries a stale copy of the toolkit
// unless someone edits it by hand (tools/plugin.mjs sync repairs that).

import { chmodSync, cpSync, existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const NAME_RE = /^[a-z0-9][a-z0-9_-]{0,63}$/;
const RESERVED = new Set(['svc', 'svcd', 'mcp', 'plugin', 'core', 'system', 'svcd-plugin']);
const TEXT_FILE = /\.(json|rs|toml|html|mjs|sh|md|txt)$/;

function fail(message) {
  console.error(`scaffold: ${message}`);
  process.exit(1);
}

function parseArgs(argv) {
  const options = { name: null, lang: null, desc: null, force: false, dir: 'plugins' };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--lang' || arg === '--desc' || arg === '--dir') {
      const value = argv[index + 1];
      if (!value) fail(`${arg} needs a value`);
      options[arg.slice(2)] = value;
      index += 1;
    } else if (arg === '--force') {
      options.force = true;
    } else if (arg.startsWith('--')) {
      fail(`unknown option ${arg}`);
    } else if (options.name === null) {
      options.name = arg;
    } else {
      fail(`unexpected argument ${arg}`);
    }
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
        // Windows: the executable bit is carried by git, not by the filesystem.
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
    const target = join(lib, 'svcd-plugin.sh');
    cpSync(join(ROOT, 'sdk', 'shell', 'svcd-plugin.sh'), target);
    try {
      chmodSync(target, 0o644); // sourced, not executed
    } catch {}
  }
  // The bin directory is created by the build (rust) or holds the committed
  // script (shell). Never leave a placeholder file in here: the packer has no
  // ignore rules, so anything in package/ ships.
  mkdirSync(join(pkg, 'payload', 'bin'), { recursive: true });
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  if (!options.name) fail('usage: node tools/scaffold.mjs <name> --lang rust|shell');
  if (!options.lang) fail('--lang is required (rust or shell)');
  if (!NAME_RE.test(options.name)) {
    fail(`"${options.name}" is not a valid plugin name: [a-z0-9][a-z0-9_-]{0,63}, no dots`);
  }
  if (RESERVED.has(options.name)) fail(`"${options.name}" is reserved`);

  const template = join(ROOT, 'templates', options.lang);
  if (!existsSync(template)) fail(`no template for --lang ${options.lang}`);

  const target = resolve(ROOT, options.dir, options.name);
  if (existsSync(target) && !options.force) {
    fail(`${target} already exists (use --force to overwrite)`);
  }

  const vars = {
    name: options.name,
    pascal: pascal(options.name),
    desc: options.desc ?? `${options.name} plugin`,
  };
  instantiate(template, target, vars);
  vendorSdk(target, options.lang);
  console.log(`created ${target}`);
  console.log('next: node tools/plugin.mjs dev');
}

main();
