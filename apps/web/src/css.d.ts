// Side-effect CSS imports (e.g. `import '@xyflow/svelte/dist/style.css'`)
// carry no values; the bundler extracts them. This ambient declaration
// just lets svelte-check/TS resolve the import. Mirrors svg.d.ts.
declare module '*.css';
