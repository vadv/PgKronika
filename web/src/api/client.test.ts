import { afterEach, expect, test, vi } from "vitest";
import { ApiError, apiFetch } from "./client";

afterEach(() => vi.unstubAllGlobals());

test("returns parsed json on 200", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(
    new Response('{"revision":1}', { status: 200 }),
  ));
  await expect(apiFetch<{ revision: number }>("/v1/ui/catalog?source=x"))
    .resolves.toEqual({ revision: 1 });
});

test("maps problem+json to ApiError", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(
    new Response('{"code":"unknown_source","title":"no such source"}', {
      status: 404,
      headers: { "content-type": "application/problem+json" },
    }),
  ));
  const err = await apiFetch("/v1/x").catch((e: unknown) => e);
  expect(err).toBeInstanceOf(ApiError);
  expect((err as ApiError).code).toBe("unknown_source");
});
