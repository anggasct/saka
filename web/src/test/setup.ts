import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

import { initSync } from "@/wasm/pkg/saka_wasm";

// Tests run the real lint: the module built by `cargo xtask wasm`, loaded from disk.
initSync({
  module: readFileSync(resolve("src/wasm/pkg/saka_wasm_bg.wasm")),
});

afterEach(() => {
  cleanup();
});
