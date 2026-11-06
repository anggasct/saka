import type { QueryClient } from "@tanstack/react-query";
import {
  Link,
  Outlet,
  createRootRouteWithContext,
  useLocation,
} from "@tanstack/react-router";

import { AuthNav } from "@/features/auth/auth-nav";
import { OfflineBanner } from "@/features/connection/offline-banner";
import { InboxNavLink } from "@/features/inbox/nav-link";
import { LanguageSelect } from "@/i18n/language-select";
import { initialLocale } from "@/i18n/locale";
import { useT } from "@/i18n/use-t";
import { platform } from "@/platform";
import { Providers } from "@/providers";

export type RouterContext = { queryClient: QueryClient };

const BARE_PATHS = ["/login", "/setup"];

function Layout() {
  const t = useT();
  const pathname = useLocation({ select: (l) => l.pathname });
  const bare = BARE_PATHS.includes(pathname);
  return (
    <div className="min-h-dvh">
      <a
        href="#content"
        className="sr-only focus:not-sr-only focus:absolute focus:m-2 focus:rounded focus:bg-background focus:p-2"
      >
        {t("app.skipToContent")}
      </a>
      <header className="border-b border-border">
        <nav className="mx-auto flex max-w-4xl flex-wrap items-center gap-x-4 px-4">
          <span className="py-3 font-semibold">{t("app.name")}</span>
          {!bare && (
            <>
              <InboxNavLink />
              <Link
                to="/docs"
                className="inline-flex min-h-11 items-center text-sm [&.active]:font-semibold [&.active]:underline"
              >
                {t("nav.docs")}
              </Link>
              <Link
                to="/delivery"
                className="inline-flex min-h-11 items-center text-sm [&.active]:font-semibold [&.active]:underline"
              >
                {t("nav.delivery")}
              </Link>
              <Link
                to="/sessions"
                className="inline-flex min-h-11 items-center text-sm [&.active]:font-semibold [&.active]:underline"
              >
                {t("nav.sessions")}
              </Link>
              <Link
                to="/settings"
                className="inline-flex min-h-11 items-center text-sm [&.active]:font-semibold [&.active]:underline"
              >
                {t("nav.settings")}
              </Link>
              {platform.connections() !== null && (
                <Link
                  to="/connections"
                  className="inline-flex min-h-11 items-center text-sm [&.active]:font-semibold [&.active]:underline"
                >
                  {t("nav.connections")}
                </Link>
              )}
              <AuthNav />
            </>
          )}
          <LanguageSelect />
        </nav>
      </header>
      <OfflineBanner />
      <main id="content">
        <Outlet />
      </main>
    </div>
  );
}

function Root() {
  const { queryClient } = Route.useRouteContext();
  return (
    <Providers queryClient={queryClient} locale={initialLocale()}>
      <Layout />
    </Providers>
  );
}

export const Route = createRootRouteWithContext<RouterContext>()({
  component: Root,
});
