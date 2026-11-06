import { ApiError } from "@/api/client";

const SIGN_IN_CODES = ["auth_required", "auth_invalid"];
const AUTH_PATHS = ["/login", "/setup"];

export function needsSignIn(error: unknown): boolean {
  return (
    error instanceof ApiError &&
    error.status === 401 &&
    SIGN_IN_CODES.includes(error.code)
  );
}

export function returnPathFor(href: string): string | null {
  const path = href.split(/[?#]/, 1)[0] ?? "";
  return AUTH_PATHS.includes(path) ? null : href;
}

export type SignInRouter = {
  state: { location: { href: string } };
  navigate: (options: {
    to: "/login";
    search: { next: string };
  }) => Promise<void>;
};

export function signInAgain(router: SignInRouter): (error: unknown) => void {
  return (error) => {
    if (!needsSignIn(error)) {
      return;
    }
    const next = returnPathFor(router.state.location.href);
    if (next !== null) {
      void router.navigate({ to: "/login", search: { next } });
    }
  };
}
