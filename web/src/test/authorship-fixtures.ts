import type {
  Authorization,
  Authorship,
  UserLink,
} from "@/features/settings/queries";

import { PROJECT_ID } from "./fixtures";
import { PROJECT } from "./settings-fixtures";

export const AUTHORSHIP = `${PROJECT}/authorship`;
export const USER_TOKEN = `${AUTHORSHIP}/user-token`;
export const LINK = "/api/v1/scm/github/user-link";
export const AUTHORIZATIONS = `${LINK}/authorizations`;
export const AUTHORIZATION_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344aa";
export const AUTHORIZATION = `${AUTHORIZATIONS}/${AUTHORIZATION_ID}`;

export const botAuthorship: Authorship = {
  project_id: PROJECT_ID,
  mode: "bot",
  mode_updated_at: null,
  author: {
    source: "saka",
    name: "Saka",
    email: "saka@users.noreply.github.com",
    login: null,
  },
  integrations: [{ integration_id: "github", user_link_state: "missing" }],
  user_token: null,
};

export function soon(days: number): string {
  return new Date(Date.now() + days * 24 * 60 * 60 * 1000).toISOString();
}

export const userLink: UserLink = {
  id: "0193f0c4-1b2a-7c3d-9e8f-0011223344bb",
  integration_id: "github",
  state: "active",
  relink_cause: null,
  provider_user_id: 1,
  login: "octo",
  name: "Octo Cat",
  email: "1+octo@users.noreply.github.com",
  linked_at: "2026-11-03T10:00:00.000Z",
  refreshed_at: null,
  access_expires_at: "2026-11-03T18:00:00.000Z",
  refresh_expires_at: "2027-05-04T10:00:00.000Z",
};

export function authorization(
  state: string,
  extra: Partial<Authorization> = {},
): Authorization {
  return {
    id: AUTHORIZATION_ID,
    integration_id: "github",
    state,
    user_code: "WDJB-MJHT",
    verification_uri: "https://github.com/login/device",
    interval_secs: 5,
    expires_at: soon(0.01),
    failure: null,
    link: null,
    ...extra,
  };
}
