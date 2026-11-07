import { useRef, useState } from "react";

import { ApiError, newIdempotencyKey } from "@/api/client";

export function useRetainedKey() {
  const retained = useRef<{ key: string; fingerprint: string } | null>(null);

  const take = (fingerprint: string): string => {
    const kept = retained.current;
    retained.current = null;
    return kept?.fingerprint === fingerprint ? kept.key : newIdempotencyKey();
  };

  // The server stores every answer it gave under the key, so only a request
  // that never reached it may be sent again with the same key.
  const keep = (key: string, fingerprint: string, error: unknown) => {
    if (!(error instanceof ApiError)) {
      retained.current = { key, fingerprint };
    }
  };

  const forget = () => {
    retained.current = null;
  };

  return { take, keep, forget };
}

export function useKeyedRun() {
  const keys = useRetainedKey();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const run = async (
    fingerprint: string,
    send: (idempotencyKey: string) => Promise<void>,
  ): Promise<boolean> => {
    const key = keys.take(fingerprint);
    setPending(true);
    setError(null);
    try {
      await send(key);
      return true;
    } catch (e) {
      setError(e);
      keys.keep(key, fingerprint, e);
      return false;
    } finally {
      setPending(false);
    }
  };

  return { run, pending, error };
}
