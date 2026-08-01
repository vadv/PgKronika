import createClient from "openapi-fetch";
import type {
  HasRequiredKeys,
  PathsWithMethod,
} from "openapi-typescript-helpers";
import type { paths } from "./schema";

/** Machine-readable API failure (wire body `{code, params}`). */
export class ApiError extends Error {
  constructor(
    public readonly code: string,
    public readonly status: number,
    public readonly params?: Record<string, unknown>,
  ) {
    super(code);
    this.name = "ApiError";
  }

  /** Seconds the server asked us to wait before retrying (503 transients). */
  get retryAfterSeconds(): number | null {
    const value = this.params?.retry_after_seconds;
    return typeof value === "number" && value > 0 ? value : null;
  }
}

/**
 * A 503 is the specified warm-up/overload signal (`cold_build_overloaded`,
 * `analytic_capacity_unavailable`, always with `Retry-After`): the reader is
 * building facts, not failing. Retry through the warm-up window (observed at
 * ~8 minutes on a fresh stand) instead of flashing an error. 4xx is a typed
 * answer — never retried.
 */
export function retryApiRequest(failureCount: number, error: unknown): boolean {
  if (error instanceof ApiError) {
    if (error.status === 503) return failureCount < 60;
    if (error.status >= 400 && error.status < 500) return false;
  }
  return failureCount < 3;
}

/** Retry-After as the base, growing to a 15 s ceiling: ~13 minutes of
 * patience at 60 attempts without hammering a warming reader. */
export function apiRetryDelay(attempt: number, error: unknown): number {
  const baseMs =
    error instanceof ApiError ? (error.retryAfterSeconds ?? 1) * 1000 : 1000;
  return Math.min(baseMs * (attempt + 1), 15_000);
}

/** True while a query is still retrying a 503: show "warming up", not an error. */
export function isWarmingUp(error: unknown): boolean {
  return error instanceof ApiError && error.status === 503;
}

const client = createClient<paths>({
  // openapi-fetch resolves paths through `new URL`, which requires an
  // absolute base; the SPA is always served same-origin with the API.
  baseUrl:
    typeof location === "undefined" ? "http://localhost" : location.origin,
  headers: { accept: "application/json" },
  // Defer the fetch lookup to call time (createClient captures
  // `globalThis.fetch` eagerly, which would freeze out test stubs).
  fetch: (input: RequestInfo | URL, init?: RequestInit) =>
    globalThis.fetch(input, init),
});

type GetPaths = PathsWithMethod<paths, "get">;
type GetOperation<P extends GetPaths> = paths[P] extends { get: infer Op }
  ? Op
  : never;
type GetParams<P extends GetPaths> =
  GetOperation<P> extends {
    parameters: infer Params;
  }
    ? Params
    : Record<string, never>;
// Generated parameters mark unused slots as `header?: never` etc.; such
// keys must not count as required when deciding whether the options
// argument is mandatory.
type ConcreteParams<P extends GetPaths> = {
  [
    K in keyof GetParams<P> as [GetParams<P>[K]] extends [never] ? never : K
  ]: GetParams<P>[K];
};
type OkStatus = 200 | 201 | 202 | 203 | 204 | 206 | 207 | "2XX";
type JsonBodies<Responses> = {
  [S in keyof Responses]: Responses[S] extends {
    content: { "application/json": infer D };
  }
    ? D
    : never;
}[keyof Responses];
/** Union of JSON bodies of the operation's 2xx responses. */
type GetData<P extends GetPaths> =
  GetOperation<P> extends {
    responses: infer R;
  }
    ? JsonBodies<Pick<R, Extract<keyof R, OkStatus>>>
    : never;

interface ErrorBody {
  code?: string;
  params?: Record<string, unknown>;
}

/**
 * Typed GET against the embedded web API. The path and its parameters
 * are checked against the generated OpenAPI schema (`schema.d.ts`); an
 * operation with required parameters forces the options argument at the
 * type level. Non-2xx responses throw {@link ApiError} carrying the wire
 * `code` and `params`.
 */
export async function apiGet<P extends GetPaths>(
  path: P,
  ...args: [HasRequiredKeys<ConcreteParams<P>>] extends [never]
    ? [options?: { params: GetParams<P> }]
    : [options: { params: GetParams<P> }]
): Promise<GetData<P>> {
  const options = args[0];
  // The call-site generics above already constrain path and params to
  // the schema; the cast only bridges openapi-fetch's internal
  // conditional signature, which cannot be satisfied from generic code.
  const { data, error, response } = await (
    client.GET as (
      p: P,
      o?: { params: GetParams<P> },
    ) => Promise<{
      data?: GetData<P>;
      error?: ErrorBody;
      response: Response;
    }>
  )(path, options);
  if (error !== undefined) {
    throw new ApiError(
      error.code ?? "http_error",
      response.status,
      error.params,
    );
  }
  if (data === undefined) {
    throw new ApiError("empty_response", response.status);
  }
  return data;
}
