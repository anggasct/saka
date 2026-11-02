import { useEffect, useState } from "react";
import { useIntl } from "react-intl";
import type { IntlShape } from "react-intl";

const MINUTE = 60;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

export function relativeTime(
  intl: IntlShape,
  iso: string,
  now: number,
): string {
  const seconds = Math.round((Date.parse(iso) - now) / 1000);
  const size = Math.abs(seconds);
  if (size < MINUTE) {
    return intl.formatRelativeTime(seconds, "second", { numeric: "auto" });
  }
  if (size < HOUR) {
    return intl.formatRelativeTime(Math.round(seconds / MINUTE), "minute");
  }
  if (size < DAY) {
    return intl.formatRelativeTime(Math.round(seconds / HOUR), "hour");
  }
  return intl.formatRelativeTime(Math.round(seconds / DAY), "day");
}

function unit(intl: IntlShape, value: number, name: string): string {
  return intl.formatNumber(value, {
    style: "unit",
    unit: name,
    unitDisplay: "long",
  });
}

export function duration(intl: IntlShape, ms: number): string {
  const total = Math.max(0, Math.round(ms / 1000));
  if (total < MINUTE) {
    return unit(intl, total, "second");
  }
  if (total < HOUR) {
    return unit(intl, Math.floor(total / MINUTE), "minute");
  }
  const hours = Math.floor(total / HOUR);
  const minutes = Math.floor((total % HOUR) / MINUTE);
  return minutes === 0
    ? unit(intl, hours, "hour")
    : `${unit(intl, hours, "hour")} ${unit(intl, minutes, "minute")}`;
}

const TICK_MS = 30_000;

export function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => {
      setNow(Date.now());
    }, TICK_MS);
    return () => {
      clearInterval(id);
    };
  }, []);
  return now;
}

export function useFormat() {
  const intl = useIntl();
  const now = useNow();
  return {
    now,
    relative: (iso: string) => relativeTime(intl, iso, now),
    absolute: (iso: string) =>
      intl.formatDate(iso, { dateStyle: "medium", timeStyle: "medium" }),
    duration: (ms: number) => duration(intl, ms),
    percent: (value: number) =>
      intl.formatNumber(value, { style: "percent", maximumFractionDigits: 0 }),
    number: (value: number) => intl.formatNumber(value),
  };
}
