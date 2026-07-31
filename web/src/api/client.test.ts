import { afterEach, expect, test, vi } from "vitest";
import { ApiError, apiGet } from "./client";
import type { VersionResponse } from "./types";

afterEach(() => vi.unstubAllGlobals());

test("returns parsed json on 200", async () => {
  const body: VersionResponse = { api: "1.0.0", format_version: 7 };
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(
    new Response(JSON.stringify(body), {
      status: 200,
      headers: { "content-type": "application/json" },
    }),
  ));
  await expect(apiGet("/v1/version")).resolves.toEqual(body);
});

test("maps error body to ApiError", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(
    new Response('{"code":"unknown_source","params":{"source":"x"}}', {
      status: 404,
      headers: { "content-type": "application/json" },
    }),
  ));
  const err: unknown = await apiGet("/v1/version").catch((e: unknown) => e);
  expect(err).toBeInstanceOf(ApiError);
  expect((err as ApiError).code).toBe("unknown_source");
  expect((err as ApiError).status).toBe(404);
  expect((err as ApiError).params).toEqual({ source: "x" });
});

test("error without a wire code falls back to http_error", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(
    new Response("boom", { status: 502 }),
  ));
  const err: unknown = await apiGet("/v1/version").catch((e: unknown) => e);
  expect(err).toBeInstanceOf(ApiError);
  expect((err as ApiError).code).toBe("http_error");
  expect((err as ApiError).status).toBe(502);
});
