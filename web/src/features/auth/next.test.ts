import { describe, expect, it } from "vitest";

import { ApiError } from "@/api/client";

import { nextSearch, safeNext } from "./next";
import { needsSignIn, returnPathFor, signInAgain } from "./unauthorized";

describe("safeNext", () => {
  it("keeps a path on this origin, with its query", () => {
    expect(safeNext("/delivery/tasks/1?tab=review")).toBe(
      "/delivery/tasks/1?tab=review",
    );
    expect(safeNext("/docs#top")).toBe("/docs#top");
  });

  it("falls back to the inbox for anything that could leave the origin or loop", () => {
    for (const raw of [
      undefined,
      "",
      "docs",
      "//evil.example",
      "https://evil.example",
      "/\\evil.example",
      "/ok\nnext",
      "/login",
      "/setup?next=/",
      "javascript:alert(1)",
    ]) {
      expect(safeNext(raw)).toBe("/");
    }
  });

  it("leaves a missing next out of the search", () => {
    expect(nextSearch(undefined)).toEqual({});
    expect(nextSearch("/a")).toEqual({ next: "/a" });
  });
});

describe("sign-in redirect", () => {
  const unauthorized = new ApiError(401, "auth_required", {});

  it("only a 401 about credentials sends the operator to sign in", () => {
    expect(needsSignIn(unauthorized)).toBe(true);
    expect(needsSignIn(new ApiError(401, "auth_invalid", {}))).toBe(true);
    expect(needsSignIn(new ApiError(403, "origin_not_allowed", {}))).toBe(
      false,
    );
    expect(needsSignIn(new ApiError(401, "other", {}))).toBe(false);
    expect(needsSignIn(new Error("x"))).toBe(false);
  });

  it("returns to where the operator was, except from the sign-in pages", () => {
    expect(returnPathFor("/delivery?state=blocked")).toBe(
      "/delivery?state=blocked",
    );
    expect(returnPathFor("/login?next=%2F")).toBeNull();
    expect(returnPathFor("/setup")).toBeNull();
  });

  it("navigates to the sign-in page with the current location", () => {
    const calls: unknown[] = [];
    const router = {
      state: { location: { href: "/settings/devices" } },
      navigate: (options: unknown) => {
        calls.push(options);
        return Promise.resolve();
      },
    };
    signInAgain(router)(unauthorized);
    signInAgain(router)(new ApiError(500, "internal", {}));
    router.state.location.href = "/login";
    signInAgain(router)(unauthorized);
    expect(calls).toEqual([
      { to: "/login", search: { next: "/settings/devices" } },
    ]);
  });
});
