import { readFileSync, writeFileSync, mkdirSync, cpSync } from "fs";
import { join } from "path";

const outdir = join(import.meta.dir, "dist");
const srcdir = import.meta.dir;

// 1. Bundle the JS/TS entry point
const result = await Bun.build({
  entrypoints: [join(srcdir, "src/main.tsx")],
  outdir,
  minify: true,
  splitting: true,
  target: "browser",
  format: "esm",
  naming: "assets/[name]-[hash].[ext]",
});

if (!result.success) {
  console.error("Build failed:");
  for (const log of result.logs) {
    console.error(log);
  }
  process.exit(1);
}

// 2. Build CSS with Tailwind
const tailwindResult = Bun.spawnSync({
  cmd: [
    "bunx",
    "tailwindcss",
    "-i",
    join(srcdir, "src/index.css"),
    "-o",
    join(outdir, "assets/index.css"),
    "--minify",
  ],
  cwd: srcdir,
  stderr: "inherit",
  stdout: "inherit",
});

if (tailwindResult.exitCode !== 0) {
  console.error("Tailwind CSS build failed");
  process.exit(1);
}

// 3. Find generated JS entry file
const jsEntry = result.outputs.find(
  (o) => o.kind === "entry-point" && o.path.endsWith(".js"),
);
if (!jsEntry) {
  console.error("No JS entry file found in build output");
  process.exit(1);
}

const jsFilename = jsEntry.path.split("/").pop()!;

// 4. Process index.html — replace script/css references with built assets
let html = readFileSync(join(srcdir, "index.html"), "utf-8");
html = html
  .replace(
    '<script type="module" src="/src/main.tsx"></script>',
    `<link rel="stylesheet" href="/assets/index.css">\n    <script type="module" src="/assets/${jsFilename}"></script>`,
  )
  .replace(
    '<link rel="icon" type="image/svg+xml" href="/vite.svg" />',
    '<link rel="icon" type="image/svg+xml" href="/favicon.svg" />',
  );

writeFileSync(join(outdir, "index.html"), html);

// 5. Copy static assets
try {
  mkdirSync(outdir, { recursive: true });
} catch {}
// Copy favicon if it exists
try {
  cpSync(join(srcdir, "public"), outdir, { recursive: true });
} catch {}

console.log(`Build complete: ${result.outputs.length} files`);
for (const output of result.outputs) {
  const size = (output.size / 1024).toFixed(1);
  console.log(`  ${output.path.replace(outdir, "dist")} (${size} KB)`);
}
