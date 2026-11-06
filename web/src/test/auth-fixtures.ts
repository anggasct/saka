export const STATUS = "/api/v1/auth/status";
export const DEVICES = "/api/v1/auth/devices";
export const PAIRINGS = "/api/v1/auth/pairings";
export const LOGIN_START = "/api/v1/auth/login/start";
export const LOGIN_FINISH = "/api/v1/auth/login/finish";
export const REGISTER_START = "/api/v1/auth/passkeys/register/start";
export const REGISTER_FINISH = "/api/v1/auth/passkeys/register/finish";
export const LOGOUT = "/api/v1/auth/logout";

export const PASSKEY_ACTOR =
  "operator:passkey:0193f0c4-0000-7000-8000-0000000000c1";

export function status(over: Record<string, unknown> = {}) {
  return {
    enabled: true,
    setup_required: false,
    authenticated: false,
    actor: null,
    ...over,
  };
}

export const SIGNED_IN = status({ authenticated: true, actor: PASSKEY_ACTOR });

export const OPTIONS = { publicKey: { challenge: "AQID" } };

export function ceremony(id = "ceremony-1") {
  return { ceremony_id: id, options: OPTIONS };
}

export const LAPTOP = {
  id: "0193f0c4-0000-7000-8000-0000000000c1",
  name: "Laptop",
  created_at: "2026-11-01T09:00:00.000Z",
  last_used_at: "2026-11-06T08:00:00.000Z",
};
export const PHONE = {
  id: "0193f0c4-0000-7000-8000-0000000000c2",
  name: "Phone",
  created_at: "2026-11-02T09:00:00.000Z",
  last_used_at: null,
};

export const LOCAL_CLI = {
  id: "0193f0c4-0000-7000-8000-0000000000d1",
  name: "Local CLI",
  kind: "local_cli",
  created_at: "2026-11-06T07:00:00.000Z",
  last_used_at: "2026-11-06T08:30:00.000Z",
  revoked_at: null,
};
export const PIXEL = {
  id: "0193f0c4-0000-7000-8000-0000000000d2",
  name: "Pixel app",
  kind: "client",
  created_at: "2026-11-05T07:00:00.000Z",
  last_used_at: null,
  revoked_at: null,
};

export const DEVICE_LIST = {
  devices: [LOCAL_CLI, PIXEL],
  credentials: [LAPTOP, PHONE],
};
