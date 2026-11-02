import type { Heading } from "@/editor/outline";
import { useT } from "@/i18n/use-t";

const INDENT = ["pl-0", "pl-3", "pl-6", "pl-9", "pl-12", "pl-15"];

export function OutlinePanel({
  headings,
  onSelect,
}: {
  headings: readonly Heading[];
  onSelect: (from: number) => void;
}) {
  const t = useT();
  return (
    <nav aria-labelledby="docs-outline" className="space-y-2">
      <h2 id="docs-outline" className="text-sm font-semibold">
        {t("docs.outline.title")}
      </h2>
      {headings.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          {t("docs.outline.empty")}
        </p>
      ) : (
        <ul className="space-y-0.5">
          {headings.map((h) => (
            <li key={h.from} className={INDENT[h.level - 1] ?? ""}>
              <button
                type="button"
                data-level={h.level}
                className="min-h-11 w-full truncate rounded px-1 text-left text-sm hover:bg-muted lg:min-h-8"
                onClick={() => {
                  onSelect(h.from);
                }}
              >
                {h.text}
              </button>
            </li>
          ))}
        </ul>
      )}
    </nav>
  );
}
