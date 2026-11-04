import { useRef, useState } from "react";

import { ApiError, newIdempotencyKey } from "@/api/client";

export function useKeyedRun() {
  const retained = useRef<{ key: string; fingerprint: string } | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const run = async (
    fingerprint: string,
    send: (idempotencyKey: string) => Promise<void>,
  ): Promise<boolean> => {
    const kept = retained.current;
    // The server stores every answer it gave under the key, so only a request
    // that never reached it may be sent again with the same key.
    const key =
      kept?.fingerprint === fingerprint ? kept.key : newIdempotencyKey();
    retained.current = null;
    setPending(true);
    setError(null);
    try {
      await send(key);
      return true;
    } catch (e) {
      setError(e);
      if (!(e instanceof ApiError)) {
        retained.current = { key, fingerprint };
      }
      return false;
    } finally {
      setPending(false);
    }
  };

  return { run, pending, error };
}
