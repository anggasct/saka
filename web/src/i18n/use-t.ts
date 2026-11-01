import { useIntl } from "react-intl";

import type { MessageKey } from "./messages";

type Values = Record<string, string | number>;

/** A typed `formatMessage`: only catalog keys compile. */
export function useT(): (key: MessageKey, values?: Values) => string {
  const intl = useIntl();
  return (key, values) => intl.formatMessage({ id: key }, values);
}
