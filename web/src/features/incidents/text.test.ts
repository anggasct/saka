import { describe, expect, it } from "vitest";

import { httpUrl, labelEntries, severityTone, stateTone } from "./text";

describe("incident text", () => {
  it("links only http and https monitoring urls", () => {
    expect(httpUrl("https://alerts.example.com/a?b=1")).toBe(
      "https://alerts.example.com/a?b=1",
    );
    expect(httpUrl("http://alerts.example.com")).toBe(
      "http://alerts.example.com",
    );
    expect(httpUrl("javascript:alert(1)")).toBeNull();
    expect(httpUrl("data:text/html,<b>x</b>")).toBeNull();
    expect(httpUrl("ftp://alerts.example.com")).toBeNull();
    expect(httpUrl("not a url")).toBeNull();
    expect(httpUrl("")).toBeNull();
  });

  it("sorts the labels by name", () => {
    expect(labelEntries({ service: "checkout", alertname: "High" })).toEqual([
      ["alertname", "High"],
      ["service", "checkout"],
    ]);
    expect(labelEntries({})).toEqual([]);
  });

  it.each([
    ["critical", "danger"],
    ["high", "danger"],
    ["medium", "attention"],
    ["low", "neutral"],
    ["info", "neutral"],
  ])("tones a %s incident %s", (severity, tone) => {
    expect(severityTone(severity)).toBe(tone);
  });

  it.each([
    ["open", "danger"],
    ["handling", "progress"],
    ["resolved", "success"],
    ["dismissed", "neutral"],
  ])("tones a %s state %s", (state, tone) => {
    expect(stateTone(state)).toBe(tone);
  });
});
