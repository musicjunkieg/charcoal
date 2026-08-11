// Ambient module declaration for the ONE Node built-in this project's tests
// import (tokens.test.ts reads tokens.css via node:fs to check --x-rgb
// triplets against their --x hex tokens).
//
// We deliberately do NOT depend on @types/node for this. @types/node's
// index.d.ts pulls in globals.d.ts, which does `declare global { var
// process: ...; var Buffer: ...; }`. Ambient globals are program-wide in
// TypeScript — ANY file bringing @types/node into the program (even just a
// test file's `import 'node:fs'`) makes `process`/`Buffer`/etc. resolve as
// if valid in browser-side app code too, silencing exactly the class of bug
// (a Node-only global leaking into client code) type-checking exists to
// catch. Confirmed empirically: even scoping @types/node via `types: []` /
// `typeRoots: []` in tsconfig didn't help, because @sveltejs/kit's
// generated ambient.d.ts unconditionally references vite's own types,
// which unconditionally reference "node" — so merely having @types/node
// installed anywhere reachable in node_modules reintroduces the leak
// regardless of tsconfig scoping.
//
// A plain `declare module` has no such effect: it only changes what an
// explicit `import ... from 'node:fs'` resolves to, so it can't leak into
// files that don't import it. Extend this if a future test needs another
// node: built-in — do not swap it back for @types/node.
declare module 'node:fs' {
	export function readFileSync(path: string, encoding: string): string;
}
