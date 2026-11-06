import { Link } from "@tanstack/react-router";

import { AuthNav } from "@/features/auth/auth-nav";
import { InboxNavLink } from "@/features/inbox/nav-link";
import { LanguageSelect } from "@/i18n/language-select";
import { useT } from "@/i18n/use-t";

const TOP_LINK =
  "inline-flex min-h-11 items-center text-sm [&.active]:font-semibold [&.active]:underline";
const BOTTOM_LINK =
  "flex min-h-14 min-w-11 flex-1 items-center justify-center gap-1 px-1 text-xs [&.active]:bg-muted [&.active]:font-semibold";

function Brand() {
  const t = useT();
  return <span className="py-3 font-semibold">{t("app.name")}</span>;
}

export function TopNav({ bare }: { bare: boolean }) {
  const t = useT();
  return (
    <nav className="mx-auto flex max-w-4xl flex-wrap items-center gap-x-4 px-4">
      <Brand />
      {!bare && (
        <>
          <InboxNavLink className={TOP_LINK} />
          <Link to="/docs" className={TOP_LINK}>
            {t("nav.docs")}
          </Link>
          <Link to="/delivery" className={TOP_LINK}>
            {t("nav.delivery")}
          </Link>
          <Link to="/sessions" className={TOP_LINK}>
            {t("nav.sessions")}
          </Link>
          <Link to="/settings" className={TOP_LINK}>
            {t("nav.settings")}
          </Link>
          <AuthNav />
        </>
      )}
      <LanguageSelect />
    </nav>
  );
}

export function CompactHeader({ bare }: { bare: boolean }) {
  const t = useT();
  return (
    <div className="mx-auto flex max-w-4xl flex-wrap items-center gap-x-4 px-4">
      <Brand />
      {!bare && (
        <>
          <Link to="/settings" className={`${TOP_LINK} min-w-11`}>
            {t("nav.settings")}
          </Link>
          <AuthNav />
        </>
      )}
      <LanguageSelect />
    </div>
  );
}

export function BottomNav() {
  const t = useT();
  return (
    <nav
      aria-label={t("nav.main")}
      className="fixed inset-x-0 bottom-0 z-10 flex border-t border-border bg-background pb-[env(safe-area-inset-bottom)]"
    >
      <InboxNavLink className={BOTTOM_LINK} />
      <Link to="/docs" className={BOTTOM_LINK}>
        {t("nav.docs")}
      </Link>
      <Link to="/delivery" className={BOTTOM_LINK}>
        {t("nav.delivery")}
      </Link>
      <Link to="/sessions" className={BOTTOM_LINK}>
        {t("nav.sessions")}
      </Link>
    </nav>
  );
}
