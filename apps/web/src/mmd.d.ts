// `import src from '…/x.mmd' with { type: 'text' }` hands the Mermaid
// source over as a string (Bun's text loader, chosen per import by the
// attribute). TypeScript needs a module declaration to know the import
// is a string. Mirrors svg.d.ts. Only it/kbDiagrams.ts imports one.

declare module '*.mmd' {
  const source: string;
  export default source;
}
