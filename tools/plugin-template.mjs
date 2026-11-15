#!/usr/bin/env node
// plugin-template — lay a plugin project down from a template.
//
//   plugin-template wireguard                 # 交互选择语言与平台
//   plugin-template wireguard --lang rust     # 非交互
//
// This only creates files. Building and packing are `plugin-build`'s job.

import { cpSync, chmodSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createInterface } from 'node:readline/promises';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const NAME_RE = /^[a-z0-9][a-z0-9_-]{0,63}$/;
const RESERVED = new Set(['svc', 'svcd', 'mcp', 'plugin', 'core', 'system', 'svcd-plugin']);
const TEXT_FILE = /\.(json|rs|c|h|toml|html|mjs|sh|ps1|psm1|md|txt)$/;
// A template directory is also a working tree: an editor's build output must not
// travel into the project. Skipped locally, not just via .gitignore.
const SKIP_DIRS = new Set(['target', 'dist', 'node_modules', '.git', '.idea', '.vscode', 'out']);

const LANGUAGES = [
  { id: 'rust', label: 'rust       编译型，crate SDK，静态二进制' },
  { id: 'c', label: 'c          编译型，单头文件 SDK' },
  { id: 'shell', label: 'shell      POSIX sh，无编译，适合包装命令' },
  { id: 'powershell', label: 'powershell PowerShell 脚本，无编译，适合 Windows 运维' },
];

// 目标三元组取自 plugin-manager 的 docs/core-selection-spec.md §7「目标」。
const PLATFORMS = [
  { id: 'x86_64-unknown-linux-musl', label: '嵌入式 x86_64（musl 静态）' },
  { id: 'aarch64-unknown-linux-musl', label: '嵌入式 aarch64（musl 静态）' },
  { id: 'armv7-unknown-linux-musleabihf', label: '嵌入式 armv7（musl 静态）' },
  { id: 'x86_64-unknown-linux-gnu', label: 'Linux 桌面 x86_64（glibc）' },
  { id: 'x86_64-pc-windows-msvc', label: 'Windows x86_64（MSVC）' },
  { id: 'aarch64-apple-darwin', label: 'macOS arm64' },
  { id: 'x86_64-apple-darwin', label: 'macOS x86_64' },
];

function fail(message, code = 1) {
  console.error(`plugin-template: ${message}`);
  process.exit(code);
}

function usage() {
  console.error(`usage: plugin-template <name> [options]

  --lang <id>        rust | c | shell | powershell（省略则交互选择）
  --platforms <ids>  逗号分隔的目标三元组（省略则交互选择）
  --webui            带上 package/payload/web
  --desc <text>      清单里的一句话描述
  --dir <path>       创建到哪（默认 plugins）
  --force            覆盖已存在的目录`);
}

function parseArgs(argv) {
  const options = {
    name: null,
    lang: null,
    platforms: null,
    webui: false,
    desc: null,
    dir: 'plugins',
    force: false,
  };
  const takesValue = { '--lang': 'lang', '--platforms': 'platforms', '--desc': 'desc', '--dir': 'dir' };
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

function menu(title, items, { multi = false } = {}) {
  console.log(`\n${title}`);
  items.forEach((item, index) => {
    console.log(`  ${String(index + 1).padStart(2)}) ${item.label}`);
    console.log(`      ${item.id}`);
  });
  if (multi) console.log('  可多选，用逗号分隔；直接回车表示全选');
}

function parseSelection(raw, items, { multi }) {
  const text = raw.trim();
  if (text === '') return multi ? items.map((item) => item.id) : null;
  if (text.toLowerCase() === 'all') return items.map((item) => item.id);
  const indexes = text
    .split(/[,，\s]+/)
    .filter(Boolean)
    .map((part) => Number.parseInt(part, 10));
  if (indexes.some((index) => !Number.isInteger(index) || index < 1 || index > items.length)) {
    return null;
  }
  const ids = indexes.map((index) => items[index - 1].id);
  return multi ? [...new Set(ids)] : ids[0];
}

async function choose(options, hostTriple) {
  const interactive = process.stdin.isTTY && process.stdout.isTTY;

  if (!options.lang) {
    if (!interactive) fail('--lang is required when stdin is not a terminal');
    menu('选择开发语言：', LANGUAGES);
    const line = createInterface({ input: process.stdin, output: process.stdout });
    try {
      const raw = await line.question('语言编号 > ');
      options.lang = parseSelection(raw, LANGUAGES, { multi: false });
    } finally {
      line.close();
    }
    if (!options.lang) fail('无效的语言编号');
  }

  if (!options.platforms) {
    const fallback = PLATFORMS.some((platform) => platform.id === hostTriple) ? hostTriple : null;
    if (!interactive) {
      if (!fallback) fail('--platforms is required when stdin is not a terminal');
      options.platforms = [fallback];
    } else {
      menu('选择目标平台：', PLATFORMS, { multi: true });
      const line = createInterface({ input: process.stdin, output: process.stdout });
      try {
        const raw = await line.question(`平台编号${fallback ? `（默认 ${fallback}）` : ''} > `);
        options.platforms = raw.trim() === '' && fallback
          ? [fallback]
          : parseSelection(raw, PLATFORMS, { multi: true });
      } finally {
        line.close();
      }
      if (!options.platforms || options.platforms.length === 0) fail('无效的平台编号');
    }
  } else {
    options.platforms = String(options.platforms)
      .split(/[,，\s]+/)
      .filter(Boolean);
  }

  for (const id of options.platforms) {
    if (!PLATFORMS.some((platform) => platform.id === id)) {
      fail(`unknown platform "${id}" (see --help for the list)`);
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
  if (lang === 'powershell') {
    const lib = join(pkg, 'payload', 'lib');
    mkdirSync(lib, { recursive: true });
    cpSync(join(ROOT, 'sdk', 'powershell', 'SvcdPlugin.ps1'), join(lib, 'SvcdPlugin.ps1'));
  }
  if (lang === 'c') {
    const src = join(pluginDir, 'src');
    mkdirSync(src, { recursive: true });
    cpSync(join(ROOT, 'sdk', 'c', 'svcd_plugin.h'), join(src, 'svcd_plugin.h'));
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

function detectHostTriple() {
  const { platform, arch } = process;
  if (platform === 'win32' && arch === 'x64') return 'x86_64-pc-windows-msvc';
  if (platform === 'linux' && arch === 'x64') return 'x86_64-unknown-linux-musl';
  if (platform === 'linux' && arch === 'arm64') return 'aarch64-unknown-linux-musl';
  if (platform === 'darwin' && arch === 'arm64') return 'aarch64-apple-darwin';
  if (platform === 'darwin' && arch === 'x64') return 'x86_64-apple-darwin';
  return null;
}

async function main() {
  const parsed = parseArgs(process.argv.slice(2));
  if (!parsed.name) {
    usage();
    process.exit(1);
  }
  if (!NAME_RE.test(parsed.name)) {
    fail(`"${parsed.name}" is not a valid plugin name: [a-z0-9][a-z0-9_-]{0,63}, no dots`);
  }
  if (RESERVED.has(parsed.name)) fail(`"${parsed.name}" is reserved`);

  const options = await choose(parsed, detectHostTriple());
  if (!LANGUAGES.some((language) => language.id === options.lang)) {
    fail(`unknown language "${options.lang}" (${LANGUAGES.map((language) => language.id).join(', ')})`);
  }

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

  // The chosen platforms are project metadata, not package content: `package/`
  // ships to devices, this file stays in the repo.
  writeFileSync(
    join(pluginDir, '.plugin.json'),
    `${JSON.stringify(
      { name: options.name, language: options.lang, platforms: options.platforms },
      null,
      2,
    )}\n`,
  );

  console.log(`\ncreated ${pluginDir}`);
  console.log(`  language:  ${options.lang}`);
  console.log(`  platforms: ${options.platforms.join(', ')}`);
  console.log(`next: cd "${pluginDir}" && plugin-build`);
}

main();
