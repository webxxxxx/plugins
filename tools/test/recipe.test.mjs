// The driver must know no language. These tests drive it with a recipe whose
// "compiler" is `node -e`, so a pass proves the branch is on the recipe, not on
// a hardcoded toolchain.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { buildFromRecipe, readRecipe } from '../lib/build.mjs';

function scratch() {
  const root = mkdtempSync(join(tmpdir(), 'svcd-recipe-'));
  mkdirSync(join(root, 'package'), { recursive: true });
  return root;
}

function manifestAt(root, execPath) {
  const manifest = { name: 'fake', version: '1.2.3', exec: { path: execPath } };
  writeFileSync(join(root, 'package', 'manifest.json'), JSON.stringify(manifest));
  return manifest;
}

test('a recipe drives a language the driver has never heard of', () => {
  const root = scratch();
  try {
    const manifest = manifestAt(root, 'payload/bin/fake');
    mkdirSync(join(root, 'out'), { recursive: true });
    writeFileSync(
      join(root, 'build.json'),
      JSON.stringify({
        build: {
          command: [
            'node',
            '-e',
            'require("fs").writeFileSync(process.argv[1], "built")',
            'out/{name}',
          ],
          artifact: 'out/{name}',
        },
      }),
    );

    buildFromRecipe(root, manifest, readRecipe(root), 'x86_64-pc-windows-msvc');

    const payload = join(root, 'package', 'payload', 'bin', 'fake');
    assert.equal(existsSync(payload), true);
    assert.equal(readFileSync(payload, 'utf8'), 'built');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('{platform} and {version} reach both the command and the env layer', () => {
  const root = scratch();
  try {
    const manifest = manifestAt(root, 'payload/bin/probe');
    mkdirSync(join(root, 'out'), { recursive: true });
    writeFileSync(
      join(root, 'build.json'),
      JSON.stringify({
        build: {
          command: [
            'node',
            '-e',
            'require("fs").writeFileSync(process.argv[1], process.env.PROBE + "|" + process.argv[2])',
            'out/{name}',
            '{platform}',
          ],
          artifact: 'out/{name}',
          env: {
            '*': { PROBE: 'base' },
            'aarch64-unknown-linux-musl': { PROBE: 'arch-{version}' },
          },
        },
      }),
    );

    buildFromRecipe(root, manifest, readRecipe(root), 'aarch64-unknown-linux-musl');

    const payload = join(root, 'package', 'payload', 'bin', 'probe');
    assert.equal(readFileSync(payload, 'utf8'), 'arch-1.2.3|aarch64-unknown-linux-musl');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('build: null is a script plugin with nothing to compile', () => {
  const root = scratch();
  try {
    const manifest = manifestAt(root, 'payload/bin/script.sh');
    writeFileSync(join(root, 'build.json'), JSON.stringify({ build: null }));

    buildFromRecipe(root, manifest, readRecipe(root), 'x86_64-unknown-linux-musl');

    assert.equal(existsSync(join(root, 'package', 'payload')), false);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('a missing build.json means no build step', () => {
  const root = scratch();
  try {
    assert.equal(readRecipe(root), null);
    const manifest = manifestAt(root, 'payload/bin/script.sh');
    buildFromRecipe(root, manifest, readRecipe(root), 'x86_64-unknown-linux-musl');
    assert.equal(existsSync(join(root, 'package', 'payload')), false);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
