import type { Compatibility } from "@/api/meta";
import { useT } from "@/i18n/use-t";

const REASONS = {
  server_too_old: "connection.incompatible.server_too_old",
  client_too_old: "connection.incompatible.client_too_old",
  unreadable: "connection.incompatible.unreadable",
} as const;

type Incompatible = Extract<Compatibility, { status: "incompatible" }>;

export function IncompatiblePage({ problem }: { problem: Incompatible }) {
  const t = useT();
  return (
    <main className="mx-auto max-w-xl space-y-3 p-6">
      <h1 className="text-xl font-semibold">
        {t("connection.incompatible.title")}
      </h1>
      <p role="alert">
        {t(REASONS[problem.reason], {
          serverVersion: problem.serverVersion,
          apiVersion: problem.apiVersion,
        })}
      </p>
    </main>
  );
}
