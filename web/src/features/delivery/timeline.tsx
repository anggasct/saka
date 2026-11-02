import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useLayoutEffect, useRef, useState } from "react";

import { fetchOlder } from "@/api/events";
import type { Activity } from "@/api/events";
import { useT } from "@/i18n/use-t";
import { platform } from "@/platform";
import { Badge } from "@/ui/badge";
import { Button } from "@/ui/button";
import { Skeleton } from "@/ui/skeleton";

import { useFormat } from "./format";
import { useTimeline } from "./queries";
import { label, record, text } from "./text";
import type { T } from "./text";

const BOTTOM_SLACK = 48;

function systemText(t: T, activity: Activity): string {
  const code = text(record(activity.data).code);
  const named = label(t, "timeline.system", code);
  return code !== "" && named !== code ? named : activity.summary;
}

function Body({ activity }: { activity: Activity }) {
  const t = useT();
  const data = record(activity.data);
  switch (activity.kind) {
    case "system":
      return <p className="text-muted-foreground">{systemText(t, activity)}</p>;
    case "thought":
      return (
        <p className="break-words whitespace-pre-wrap text-muted-foreground italic">
          {activity.summary}
        </p>
      );
    case "action":
      return (
        <p className="flex flex-wrap items-center gap-2">
          <Badge>
            {text(data.tool) || label(t, "timeline.kind", "action")}
          </Badge>
          <code className="break-all">{activity.summary}</code>
        </p>
      );
    case "output":
      return (
        <pre
          className={`rounded p-2 text-xs break-words whitespace-pre-wrap ${data.is_error === true ? "bg-danger text-danger-foreground" : "bg-muted"}`}
        >
          {activity.summary}
        </pre>
      );
    case "permission":
      return (
        <p className="flex flex-wrap items-center gap-2">
          <code className="break-all">{activity.summary}</code>
          <Badge tone="attention">
            {label(t, "permission.state", text(data.state))}
          </Badge>
        </p>
      );
    case "result":
      return (
        <div className="space-y-1">
          <p className="font-medium">
            {t("timeline.result", {
              turn: text(data.turn),
              status: text(data.status),
            })}
          </p>
          {activity.summary !== "" && (
            <p className="break-words whitespace-pre-wrap">
              {activity.summary}
            </p>
          )}
        </div>
      );
    case "steer":
      return (
        <p className="break-words whitespace-pre-wrap">
          {t("timeline.steer", { text: activity.summary })}
        </p>
      );
    default:
      return (
        <p className="break-words whitespace-pre-wrap">{activity.summary}</p>
      );
  }
}

function Row({ activity }: { activity: Activity }) {
  const t = useT();
  const format = useFormat();
  return (
    <li className="space-y-1 border-t border-border py-2 text-sm first:border-t-0">
      <p className="flex flex-wrap gap-x-2 text-xs text-muted-foreground">
        <time dateTime={activity.at} title={format.absolute(activity.at)}>
          {format.clock(activity.at)}
        </time>
        <span>{label(t, "timeline.kind", activity.kind)}</span>
      </p>
      <Body activity={activity} />
    </li>
  );
}

export function Timeline({ sessionId }: { sessionId: string }) {
  const t = useT();
  const queryClient = useQueryClient();
  const timeline = useTimeline(sessionId);
  const logRef = useRef<HTMLDivElement>(null);
  const markerRef = useRef<HTMLDivElement>(null);
  const loadingRef = useRef(false);
  const shape = useRef({ first: -1, last: -1, height: 0 });
  const lastTop = useRef(0);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [following, setFollowing] = useState(true);
  const [seen, setSeen] = useState(-1);

  const items = timeline.data?.items ?? [];
  const first = items[0]?.offset ?? -1;
  const last = items.at(-1)?.offset ?? -1;
  const more = timeline.data?.prevBefore != null;
  const unseen = following ? 0 : items.filter((a) => a.offset > seen).length;

  const loadOlder = async () => {
    if (loadingRef.current) {
      return;
    }
    loadingRef.current = true;
    setLoadingOlder(true);
    try {
      await fetchOlder(queryClient, sessionId);
    } catch {
      return;
    } finally {
      loadingRef.current = false;
      setLoadingOlder(false);
    }
  };

  useEffect(() => {
    const marker = markerRef.current;
    if (marker === null || !more) {
      return;
    }
    return platform.observeVisible(marker, () => {
      void loadOlder();
    });
  });

  useLayoutEffect(() => {
    const log = logRef.current;
    if (log === null) {
      return;
    }
    const before = shape.current;
    if (before.first !== -1 && first < before.first) {
      log.scrollTop += log.scrollHeight - before.height;
    } else if (following && last !== before.last) {
      log.scrollTop = log.scrollHeight;
    }
    shape.current = { first, last, height: log.scrollHeight };
  }, [first, last, following]);

  const toBottom = () => {
    const log = logRef.current;
    if (log !== null) {
      log.scrollTop = log.scrollHeight;
    }
    setFollowing(true);
  };

  if (timeline.isPending) {
    return <Skeleton className="h-32" />;
  }
  if (timeline.isError) {
    return (
      <p className="text-sm text-muted-foreground">{t("timeline.error")}</p>
    );
  }
  return (
    <div className="space-y-2">
      <div
        ref={logRef}
        role="log"
        aria-label={t("timeline.label")}
        tabIndex={0}
        onScroll={(event) => {
          const log = event.currentTarget;
          const atBottom =
            log.scrollHeight - log.scrollTop - log.clientHeight <= BOTTOM_SLACK;
          const scrolledUp = log.scrollTop < lastTop.current;
          lastTop.current = log.scrollTop;
          if (atBottom && !following) {
            setFollowing(true);
          } else if (!atBottom && scrolledUp && following) {
            setFollowing(false);
            setSeen(last);
          }
        }}
        className="max-h-[60vh] overflow-y-auto rounded-md border border-border p-2 focus-visible:outline-2 focus-visible:outline-ring"
      >
        {more && (
          <div ref={markerRef} className="flex justify-center pb-2">
            <Button
              variant="outline"
              disabled={loadingOlder}
              onClick={() => {
                void loadOlder();
              }}
            >
              {t("timeline.earlier")}
            </Button>
          </div>
        )}
        {items.length === 0 ? (
          <p className="p-2 text-sm text-muted-foreground">
            {t("timeline.empty")}
          </p>
        ) : (
          <ol>
            {items.map((activity) => (
              <Row key={activity.offset} activity={activity} />
            ))}
          </ol>
        )}
      </div>
      {unseen > 0 && (
        <Button variant="outline" onClick={toBottom}>
          {t("timeline.newer", { count: unseen })}
        </Button>
      )}
    </div>
  );
}
