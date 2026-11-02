import { useId } from "react";

import { useLocale } from "./locale-context";
import { LOCALES } from "./messages";
import { useT } from "./use-t";

export function LanguageSelect() {
  const t = useT();
  const { locale, setLocale } = useLocale();
  const id = useId();
  return (
    <div className="ml-auto flex items-center gap-2">
      <label htmlFor={id} className="sr-only">
        {t("language.label")}
      </label>
      <select
        id={id}
        value={locale}
        onChange={(event) => {
          const next = LOCALES.find((l) => l === event.target.value);
          if (next !== undefined) {
            setLocale(next);
          }
        }}
        className="min-h-11 rounded-md border border-border bg-background px-2 text-sm"
      >
        <option value="en" lang="en">
          {t("language.en")}
        </option>
        <option value="id" lang="id">
          {t("language.id")}
        </option>
      </select>
    </div>
  );
}
