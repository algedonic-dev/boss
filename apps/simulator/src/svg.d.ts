// Bun's asset loader resolves `import foo from './foo.svg'` to a
// content-hashed URL string. TypeScript needs a module declaration
// to know the import returns a string rather than choking on the
// unknown file extension.

declare module '*.svg' {
  const url: string;
  export default url;
}
