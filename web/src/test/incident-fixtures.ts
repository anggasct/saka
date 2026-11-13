import type {
  Incident,
  IncidentDetail,
  IncidentSource,
} from "@/features/incidents/queries";

import { PROJECT_ID } from "./fixtures";

export const INCIDENT_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a1";
export const OTHER_INCIDENT_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a2";
export const INCIDENT_DEPLOYMENT_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a3";
export const INCIDENT_TASK_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a4";
export const INCIDENT_DRAFT_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a5";
export const INCIDENT_SPEC_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a6";
export const INCIDENT_REVERT_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a7";
export const INCIDENT_SOURCES = `/api/v1/projects/${PROJECT_ID}/incident-sources`;
export const INCIDENTS = `/api/v1/projects/${PROJECT_ID}/incidents`;
export const incidentUrl = (id: string) => `/api/v1/incidents/${id}`;

export function incident(over: Partial<Incident> = {}): Incident {
  return {
    id: INCIDENT_ID,
    project_id: PROJECT_ID,
    source: "prod-alerts",
    fingerprint: "HighErrorRate/checkout",
    state: "open",
    signal_status: "firing",
    severity: "high",
    title: "Checkout error rate above 5%",
    summary: "5xx responses on /checkout rose after the last deployment.",
    environment: "staging",
    url: "https://alerts.example.com/alert/42",
    labels: { service: "checkout", alertname: "HighErrorRate" },
    occurrences: 3,
    started_at: "2026-11-06T10:05:00.000Z",
    first_seen: "2026-11-06T10:05:00.000Z",
    last_seen: "2026-11-06T10:20:00.000Z",
    deployment: {
      id: INCIDENT_DEPLOYMENT_ID,
      state: "succeeded",
      sha: "feedface".padEnd(40, "0"),
      phase: null,
      finished_at: "2026-11-06T10:03:00.000Z",
    },
    ...over,
  };
}

export function incidentDetail(
  over: Partial<IncidentDetail> = {},
): IncidentDetail {
  return {
    ...incident(),
    range: {
      previous_sha: "c0ffee".padEnd(40, "0"),
      sha: "feedface".padEnd(40, "0"),
    },
    suspects: [
      {
        kind: "task",
        work_item: { id: INCIDENT_TASK_ID, key: "tasks-done/s1" },
      },
      {
        kind: "change",
        change: { number: 12, url: "https://github.com/acme/app/pull/12" },
      },
    ],
    ...over,
  };
}

export function incidentSource(
  over: Partial<IncidentSource> = {},
): IncidentSource {
  return {
    name: "prod-alerts",
    kind: "alertmanager",
    endpoint: "https://saka.example.com/hooks/incident/prod-alerts",
    endpoint_path: "/hooks/incident/prod-alerts",
    action: "draft",
    min_severity: "high",
    max_open: 50,
    repo: "app",
    open: 3,
    ...over,
  };
}
