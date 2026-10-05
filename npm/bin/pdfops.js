#!/usr/bin/env node
// Runs the pdfops binary, downloading the build for this platform on first use.
// Nothing is written to stdout here: over MCP, stdout belongs to the protocol.
'use strict';

const { spawnSync } = require('child_process');
const crypto = require('crypto');
const fs = require('fs');
const os = require('os');
const path = require('path');

const { version } = require('../package.json');

const TARGETS = {
  'linux-x64': 'x86_64-unknown-linux-gnu',
  'linux-arm64': 'aarch64-unknown-linux-gnu',
  'darwin-arm64': 'aarch64-apple-darwin',
  'darwin-x64': 'x86_64-apple-darwin',
  'win32-x64': 'x86_64-pc-windows-msvc',
};

function fail(message) {
  console.error(`pdfops: ${message}`);
  process.exit(1);
}

function cacheDir() {
  if (process.env.PDFOPS_CACHE) return process.env.PDFOPS_CACHE;
  const base =
    process.platform === 'win32'
      ? process.env.LOCALAPPDATA || path.join(os.homedir(), 'AppData', 'Local')
      : process.env.XDG_CACHE_HOME || path.join(os.homedir(), '.cache');
  return path.join(base, 'pdfops', version);
}

async function download(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url} answered ${response.status}`);
  return Buffer.from(await response.arrayBuffer());
}

async function install(binary) {
  const target = TARGETS[`${process.platform}-${process.arch}`];
  if (!target) {
    fail(`no prebuilt binary for ${process.platform}-${process.arch}; install with "cargo install pdfops"`);
  }
  const archive = `pdfops-v${version}-${target}.${process.platform === 'win32' ? 'zip' : 'tar.gz'}`;
  const base =
    process.env.PDFOPS_RELEASE_URL || `https://github.com/KayZedd/pdfops/releases/download/v${version}`;
  console.error(`pdfops: downloading ${archive}`);
  const [bytes, sums] = await Promise.all([download(`${base}/${archive}`), download(`${base}/SHA256SUMS`)]);

  // The release publishes checksums; a download that does not match is never run.
  const expected = sums
    .toString()
    .split('\n')
    .map((line) => line.trim().split(/\s+/))
    .find((parts) => parts[1] === archive);
  const actual = crypto.createHash('sha256').update(bytes).digest('hex');
  if (!expected || expected[0] !== actual) {
    fail(`checksum mismatch for ${archive}; refusing to run it`);
  }

  const dir = path.dirname(binary);
  fs.mkdirSync(dir, { recursive: true });
  const scratch = fs.mkdtempSync(path.join(dir, 'unpack-'));
  try {
    const file = path.join(scratch, archive);
    fs.writeFileSync(file, bytes);
    // tar ships with Linux, macOS and Windows 10+, where it also reads zip files.
    const unpacked = spawnSync('tar', ['-xf', file, '-C', scratch], { stdio: ['ignore', 'ignore', 'inherit'] });
    if (unpacked.status !== 0) fail(`cannot unpack ${archive}: is "tar" installed?`);
    const built = path.join(scratch, `pdfops-v${version}-${target}`, path.basename(binary));
    fs.chmodSync(built, 0o755);
    // Renamed into place, so a second process starting meanwhile never sees half a file.
    fs.renameSync(built, binary);
  } finally {
    fs.rmSync(scratch, { recursive: true, force: true });
  }
}

async function main() {
  let binary = process.env.PDFOPS_BINARY;
  if (!binary) {
    binary = path.join(cacheDir(), process.platform === 'win32' ? 'pdfops.exe' : 'pdfops');
    if (!fs.existsSync(binary)) {
      await install(binary);
    }
  }
  const result = spawnSync(binary, process.argv.slice(2), { stdio: 'inherit' });
  if (result.error) fail(`cannot run ${binary}: ${result.error.message}`);
  process.exit(result.status === null ? 1 : result.status);
}

main().catch((error) => fail(error.message));
