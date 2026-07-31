import { afterEach, expect, test, vi } from "vitest";
import { ApiError, apiFetch } from "./client";

afterEach(() => vi.unstubAllGlobals());

function stubResponse(body: string, init: ResponseInit) {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(body, init)));
}

test("returns parsed json on 200", async () => {
  stubResponse('{"revision":1}', { status: 200 });
  await expect(
    apiFetch<{ revision: number }>("/v1/ui/catalog?source=x"),
  ).resolves.toEqual({ revision: 1 });
});

test("maps application/json error body to ApiError", async () => {
  stubResponse('{"code":"unknown_source","params":{"source":"x"}}', {
    status: 404,
    headers: { "content-type": "application/json" },
  });
  const err = await apiFetch("/v1/x").catch((e: unknown) => e);
  expect(err).toBeInstanceOf(ApiError);
  expect((err as ApiError).code).toBe("unknown_source");
  expect((err as ApiError).params).toEqual({ source: "x" });
});

test("maps problem+json error body to ApiError", async () => {
  stubResponse('{"code":"range_too_wide","params":{}}', {
    status: 400,
    headers: { "content-type": "application/problem+json" },
  });
  const err = await apiFetch("/v1/x").catch((e: unknown) => e);
  expect((err as ApiError).code).toBe("range_too_wide");
});

test("falls back to http_error when the body is not ours", async () => {
  stubResponse("<html>502 Bad Gateway</html>", {
    status: 502,
    headers: { "content-type": "text/html" },
  });
  const err = await apiFetch("/v1/x").catch((e: unknown) => e);
  expect((err as ApiError).code).toBe("http_error");
  expect((err as ApiError).status).toBe(502);
});

test("falls back to http_error when json is malformed", async () => {
  stubResponse("{not json", {
    status: 500,
    headers: { "content-type": "application/json" },
  });
  const err = await apiFetch("/v1/x").catch((e: unknown) => e);
  expect((err as ApiError).code).toBe("http_error");
});
