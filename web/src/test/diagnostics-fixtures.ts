import type {
  DiagnosticCheck,
  DiagnosticReport,
} from "@/features/settings/queries";

export const DIAGNOSTICS = "/api/v1/diagnostics";

function check(
  id: string,
  status: string,
  code: string,
  params: object = {},
  fix: DiagnosticCheck["fix"] = null,
  label = "instance",
): DiagnosticCheck {
  return {
    id,
    status,
    code,
    params,
    fix,
    duration_ms: 3,
    subject: { kind: "instance", id: "instance", label },
  };
}

export const report: DiagnosticReport = {
  mode: "online",
  server_version: "0.1.0",
  generated_at: "2026-11-05T10:00:00.000Z",
  summary: { pass: 2, warn: 1, fail: 1, skip: 1 },
  checks: [
    check("server.ready", "pass", "server_ready"),
    check(
      "agent.login",
      "fail",
      "agent_not_logged_in",
      { account: "claude:default" },
      { code: "run_agent_login", params: { command: "saka agent login" } },
      "claude:default",
    ),
    check("toolchain.cache", "pass", "toolchain_cache_ok", {
      path: "/var/saka/toolchains",
    }),
    check(
      "project.config",
      "warn",
      "path_scope_overlap",
      { repo: "web", other_project: "shop", other_repo: "app" },
      { code: "narrow_path_scope", params: { repo: "web" } },
      "tasks",
    ),
    check("scm.webhook", "skip", "webhook_not_configured", {}, null, "github"),
  ],
};

export const unknownReport: DiagnosticReport = {
  ...report,
  summary: { pass: 0, warn: 0, fail: 1, skip: 0 },
  checks: [check("future.check", "fail", "something_new")],
};
