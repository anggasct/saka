import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import "@testing-library/jest-dom/vitest";
import { cleanup, configure } from "@testing-library/react";
import { afterEach } from "vitest";

import { initSync } from "@/wasm/pkg/saka_wasm";

// Tests run the real lint: the module built by `cargo xtask wasm`, loaded from disk.
initSync({
  module: readFileSync(resolve("src/wasm/pkg/saka_wasm_bg.wasm")),
});

// The first render of the app in a test file costs about 0.4 s of CPU, and 24 parallel runs
// stretch it past the 1 s default; waits end as soon as the DOM is ready, so only a failure waits it out.
configure({ asyncUtilTimeout: 4000 });

// jsdom has no scrolling or layout: the router restores scroll positions, and
// CodeMirror measures text ranges after each update. Node-only suites skip this.
if (typeof window !== "undefined") {
  window.scrollTo = () => undefined;
  const noRects = {
    length: 0,
    item: () => null,
    [Symbol.iterator]: [][Symbol.iterator],
  };
  Range.prototype.getClientRects = () => noRects;
  Range.prototype.getBoundingClientRect = () => new DOMRect();
}

afterEach(() => {
  cleanup();
});
