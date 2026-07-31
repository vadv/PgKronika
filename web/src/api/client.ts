export class ApiError extends Error {
  constructor(
    public readonly code: string,
    public readonly status: number,
    public readonly params: Record<string, unknown> = {},
  ) {
    super(code);
    this.name = "ApiError";
  }
}

/**
 * `ApiError` as declared in `bins/pg_kronika-web/openapi/`. There is no
 * human-readable message on the wire: `params` carries the specifics (which
 * parameter, what was expected) and the UI renders `code` through i18n.
 */
interface ApiErrorBody {
  code: string;
  params: Record<string, unknown>;
}

function isApiErrorBody(value: unknown): value is ApiErrorBody {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { code?: unknown }).code === "string"
  );
}

/**
 * Keyed on `json` rather than an exact media type: the server answers
 * `application/json`, the v6 target is `application/problem+json`, and a client
 * that recognizes only one of them silently discards every error code.
 */
async function readErrorBody(res: Response): Promise<ApiErrorBody | null> {
  if (!res.headers.get("content-type")?.includes("json")) {
    return null;
  }
  const body: unknown = await res.json().catch(() => null);
  return isApiErrorBody(body) ? body : null;
}

export async function apiFetch<T>(path: string): Promise<T> {
  const res = await fetch(path, { headers: { accept: "application/json" } });
  if (!res.ok) {
    const body = await readErrorBody(res);
    throw new ApiError(body?.code ?? "http_error", res.status, body?.params);
  }
  return (await res.json()) as T;
}
