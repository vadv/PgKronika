export class ApiError extends Error {
  constructor(
    public readonly code: string,
    public readonly status: number,
    detail?: string,
  ) {
    super(detail ?? code);
    this.name = "ApiError";
  }
}

interface ProblemJson {
  code?: string;
  title?: string;
  detail?: string;
}

export async function apiFetch<T>(path: string): Promise<T> {
  const res = await fetch(path, { headers: { accept: "application/json" } });
  if (!res.ok) {
    let problem: ProblemJson = {};
    if (res.headers.get("content-type")?.includes("problem+json")) {
      problem = (await res.json()) as ProblemJson;
    }
    throw new ApiError(
      problem.code ?? "http_error",
      res.status,
      problem.detail ?? problem.title,
    );
  }
  return (await res.json()) as T;
}
