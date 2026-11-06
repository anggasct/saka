import type { CredentialResult, Credentials } from "./index";

type Json = Record<string, unknown>;

function isRecord(value: unknown): value is Json {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function decode(text: string): ArrayBuffer {
  const padded = text.replaceAll("-", "+").replaceAll("_", "/");
  const binary = atob(padded.padEnd(Math.ceil(padded.length / 4) * 4, "="));
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes.buffer;
}

function encode(buffer: ArrayBuffer): string {
  let binary = "";
  for (const byte of new Uint8Array(buffer)) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary)
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replaceAll("=", "");
}

function descriptors(value: unknown): PublicKeyCredentialDescriptor[] {
  if (!Array.isArray(value)) {
    return [];
  }
  return value.filter(isRecord).map((d) => ({
    ...(d as unknown as PublicKeyCredentialDescriptor),
    id: decode(String(d.id)),
  }));
}

function publicKey(options: unknown): Json | null {
  return isRecord(options) && isRecord(options.publicKey)
    ? options.publicKey
    : null;
}

function creationOptions(options: unknown): CredentialCreationOptions | null {
  const key = publicKey(options);
  if (key === null || !isRecord(key.user)) {
    return null;
  }
  return {
    publicKey: {
      ...(key as unknown as PublicKeyCredentialCreationOptions),
      challenge: decode(String(key.challenge)),
      user: {
        ...(key.user as unknown as PublicKeyCredentialUserEntity),
        id: decode(String(key.user.id)),
      },
      excludeCredentials: descriptors(key.excludeCredentials),
    },
  };
}

function requestOptions(options: unknown): CredentialRequestOptions | null {
  const key = publicKey(options);
  if (key === null) {
    return null;
  }
  return {
    publicKey: {
      ...(key as unknown as PublicKeyCredentialRequestOptions),
      challenge: decode(String(key.challenge)),
      allowCredentials: descriptors(key.allowCredentials),
    },
  };
}

function attestation(credential: PublicKeyCredential): Json {
  const response = credential.response as AuthenticatorAttestationResponse;
  return {
    id: credential.id,
    rawId: encode(credential.rawId),
    type: credential.type,
    response: {
      attestationObject: encode(response.attestationObject),
      clientDataJSON: encode(response.clientDataJSON),
    },
    extensions: {},
  };
}

function assertion(credential: PublicKeyCredential): Json {
  const response = credential.response as AuthenticatorAssertionResponse;
  return {
    id: credential.id,
    rawId: encode(credential.rawId),
    type: credential.type,
    response: {
      authenticatorData: encode(response.authenticatorData),
      clientDataJSON: encode(response.clientDataJSON),
      signature: encode(response.signature),
      userHandle:
        response.userHandle === null ? null : encode(response.userHandle),
    },
    extensions: {},
  };
}

async function ask(
  call: (container: CredentialsContainer) => Promise<Credential | null>,
  json: (credential: PublicKeyCredential) => Json,
): Promise<CredentialResult> {
  if (typeof navigator.credentials === "undefined") {
    return { status: "unsupported" };
  }
  try {
    const credential = await call(navigator.credentials);
    if (credential === null) {
      return { status: "cancelled" };
    }
    return {
      status: "ok",
      credential: json(credential as PublicKeyCredential),
    };
  } catch (error) {
    if (error instanceof DOMException && error.name === "NotAllowedError") {
      return { status: "cancelled" };
    }
    if (error instanceof DOMException && error.name === "InvalidStateError") {
      return { status: "duplicate" };
    }
    return { status: "failed" };
  }
}

export const webCredentials: Credentials = {
  supported: () =>
    typeof navigator.credentials !== "undefined" &&
    typeof PublicKeyCredential !== "undefined",
  create: async (options) => {
    const parsed = creationOptions(options);
    if (parsed === null) {
      return { status: "failed" };
    }
    return ask((container) => container.create(parsed), attestation);
  },
  get: async (options) => {
    const parsed = requestOptions(options);
    if (parsed === null) {
      return { status: "failed" };
    }
    return ask((container) => container.get(parsed), assertion);
  },
};
