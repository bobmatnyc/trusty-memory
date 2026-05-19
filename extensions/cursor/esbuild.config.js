// Why: VS Code extensions ship a single CommonJS bundle; esbuild gives us a
// fast, dependency-light build without committing to a heavier toolchain.
// What: Bundles src/extension.ts into dist/extension.js, externalizing the
// `vscode` module (provided by the host at runtime). Supports a --watch flag.
// Test: `npm run build` must exit 0 and produce dist/extension.js; CI runs
// `npm run typecheck` separately for strict type verification.
'use strict';

const esbuild = require('esbuild');

const watch = process.argv.includes('--watch');

/** @type {import('esbuild').BuildOptions} */
const options = {
  entryPoints: ['src/extension.ts'],
  bundle: true,
  outfile: 'dist/extension.js',
  platform: 'node',
  target: 'node18',
  format: 'cjs',
  // `vscode` is injected by the extension host and must not be bundled.
  external: ['vscode'],
  sourcemap: true,
  minify: !watch,
  logLevel: 'info',
};

async function main() {
  if (watch) {
    const ctx = await esbuild.context(options);
    await ctx.watch();
    console.log('[esbuild] watching for changes...');
  } else {
    await esbuild.build(options);
    console.log('[esbuild] build complete');
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
