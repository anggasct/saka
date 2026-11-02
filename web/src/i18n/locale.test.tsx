import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { platform } from "@/platform";
import { renderWithProviders } from "@/test/render";

import { LanguageSelect } from "./language-select";
import { initialLocale } from "./locale";
import { useT } from "./use-t";

function Sample() {
  const t = useT();
  return (
    <>
      <LanguageSelect />
      <p>{t("delivery.noTasks")}</p>
    </>
  );
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("locale", () => {
  it("starts from the browser languages when nothing is saved", () => {
    vi.spyOn(platform.preferences, "get").mockReturnValue(null);
    vi.spyOn(platform, "preferredLocales").mockReturnValue(["id-ID", "en"]);
    expect(initialLocale()).toBe("id");
  });

  it("prefers the saved language and ignores one it does not know", () => {
    vi.spyOn(platform, "preferredLocales").mockReturnValue(["en-US"]);
    const get = vi.spyOn(platform.preferences, "get").mockReturnValue("id");
    expect(initialLocale()).toBe("id");
    get.mockReturnValue("fr");
    expect(initialLocale()).toBe("en");
  });

  it("switches the catalog, the document language, and the saved choice", async () => {
    const setLanguage = vi.spyOn(platform, "setDocumentLanguage");
    const save = vi.spyOn(platform.preferences, "set");
    renderWithProviders(<Sample />);
    expect(
      await screen.findByText("No tasks for this spec yet."),
    ).toBeInTheDocument();
    expect(setLanguage).toHaveBeenLastCalledWith("en");
    await userEvent.selectOptions(
      screen.getByRole("combobox", { name: "Language" }),
      "Bahasa Indonesia",
    );
    expect(
      await screen.findByText("Belum ada task untuk spec ini."),
    ).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Bahasa" })).toHaveValue("id");
    expect(setLanguage).toHaveBeenLastCalledWith("id");
    expect(save).toHaveBeenCalledWith("locale", "id");
  });
});
