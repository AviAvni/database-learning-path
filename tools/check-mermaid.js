#!/usr/bin/env node
// Parse every ```mermaid block with the same mermaid.min.js the book ships.
//
// Why this exists
// ---------------
// The book workflow proves each diagram *reached* the renderer as a mermaid
// block. It cannot prove the diagram parses: mermaid renders a syntax error as
// a red bomb icon in the browser, after the build has passed. This runs
// mermaid's own parser in node, so a broken diagram fails here instead.
//
// Usage:  node tools/check-mermaid.js mermaid.min.js FILE.md...
//
// mermaid is a browser bundle, so the globals below are the least DOM it needs
// to load. Parsing then succeeds or throws a "Parse error" / "Lexical error";
// a later failure inside the DOM sanitiser means the parser already accepted
// the diagram, and is not a syntax error.

const fs = require("fs");
const vm = require("vm");

const noop = () => {};
globalThis.window = globalThis;
globalThis.addEventListener = noop;
for (const k of ["Element", "Node", "DocumentFragment", "HTMLTemplateElement",
  "NamedNodeMap", "HTMLFormElement", "DOMParser", "Text", "Comment", "Document"]) {
  globalThis[k] = class {};
}
globalThis.NodeFilter = {};
globalThis.document = {
  nodeType: 9,
  implementation: { createHTMLDocument: () => ({}), createDocument: () => ({}) },
  createElement: () => ({ setAttribute: noop, style: {}, appendChild: noop }),
  querySelector: () => null,
  querySelectorAll: () => [],
  addEventListener: noop,
  documentElement: { style: {} },
};

const [bundle, ...files] = process.argv.slice(2);
if (!bundle || files.length === 0) {
  console.error("usage: node tools/check-mermaid.js mermaid.min.js FILE.md...");
  process.exit(2);
}
vm.runInThisContext(fs.readFileSync(bundle, "utf8"));
const mermaid = globalThis.mermaid;
mermaid.initialize({ startOnLoad: false });

const SYNTAX = /^(Parse error|Lexical error|No diagram type)/;

(async () => {
  let total = 0;
  let bad = 0;
  for (const f of files) {
    const text = fs.readFileSync(f, "utf8");
    for (const m of text.matchAll(/^```mermaid\n([\s\S]*?)^```$/gm)) {
      total++;
      try {
        await mermaid.parse(m[1]);
      } catch (e) {
        const msg = String((e && e.message) || e);
        if (!SYNTAX.test(msg)) continue;
        bad++;
        const line = text.slice(0, m.index).split("\n").length;
        const where = `${f}:${line}`;
        const first = msg.split("\n").slice(0, 3).join(" | ");
        console.log(process.env.GITHUB_ACTIONS ? `::error file=${f},line=${line}::${first}` : `${where}: ${first}`);
      }
    }
  }
  console.log(`parsed ${total} mermaid blocks, ${bad} with syntax errors`);
  process.exit(bad ? 1 : 0);
})();
