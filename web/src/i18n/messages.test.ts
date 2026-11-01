import { describe, expect, it } from "vitest";

import { CATALOGS, missingKeys } from "./messages";

describe("message catalogs", () => {
  it("have the same keys in every locale", () => {
    expect(missingKeys(CATALOGS)).toEqual({});
  });

  it("report a key missing in one locale", () => {
    expect(missingKeys({ en: { a: "A", b: "B" }, id: { a: "A" } })).toEqual({
      id: ["b"],
    });
  });
});
