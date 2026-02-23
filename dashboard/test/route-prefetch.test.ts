import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { prefetchRouteData } from "../src/lib/route-prefetch";

type FetchCall = { url: string; method: string };

function response(data: unknown, status = 200) {
  return {
    ok: status >= 200 && status < 300,
    status,
    statusText: status >= 200 && status < 300 ? "OK" : "Error",
    json: () => Promise.resolve(data),
    text: () => Promise.resolve(JSON.stringify(data)),
    headers: new Headers({ "content-type": "application/json" }),
  };
}

async function flushAsync() {
  await Promise.resolve();
  await Promise.resolve();
  await new Promise((resolve) => setTimeout(resolve, 0));
}

describe("prefetchRouteData", () => {
  const originalFetch = globalThis.fetch;
  let calls: FetchCall[] = [];

  beforeEach(() => {
    calls = [];
    globalThis.fetch = mock((input: string | URL | Request, init?: RequestInit) => {
      const requestUrl =
        typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
      const method = init?.method ?? (input instanceof Request ? input.method : "GET");
      calls.push({ url: requestUrl, method });

      if (requestUrl.includes("/api/config")) {
        return Promise.reject(new Error("network error"));
      }

      return Promise.resolve(response({ ok: true })) as Promise<Response>;
    }) as unknown as typeof fetch;
  });

  afterEach(() => {
    globalThis.fetch = originalFetch;
  });

  test("preloads critical routes, dedupes by route, and ignores unknown routes", async () => {
    prefetchRouteData("/");
    prefetchRouteData("/issues");
    prefetchRouteData("/attempts");
    prefetchRouteData("/prs");
    prefetchRouteData("/analytics");
    prefetchRouteData("/errors");
    prefetchRouteData("/feedback");
    prefetchRouteData("/regressions");
    prefetchRouteData("/experiments");
    prefetchRouteData("/learning");
    prefetchRouteData("/repos");
    prefetchRouteData("/inference");
    prefetchRouteData("/activity");
    prefetchRouteData("/telemetry");
    prefetchRouteData("/config");
    prefetchRouteData("/users");
    prefetchRouteData("/does-not-exist");

    await flushAsync();

    const initialUserCalls = calls.filter((call) => call.url.includes("/api/users")).length;
    prefetchRouteData("/users");
    prefetchRouteData("/");
    prefetchRouteData("/does-not-exist");
    await flushAsync();

    const userCallsAfterDuplicate = calls.filter((call) => call.url.includes("/api/users")).length;
    expect(userCallsAfterDuplicate).toBe(initialUserCalls);

    expect(calls.some((call) => call.url.includes("/api/stats/overview"))).toBe(true);
    expect(calls.some((call) => call.url.includes("/api/retries"))).toBe(true);
    expect(calls.some((call) => call.url.includes("/api/issues?"))).toBe(true);
    expect(calls.some((call) => call.url.includes("/api/attempts?"))).toBe(true);
    expect(calls.some((call) => call.url.includes("/api/prs/analytics"))).toBe(true);
    expect(calls.some((call) => call.url.includes("/api/analytics/summary"))).toBe(true);
    expect(calls.some((call) => call.url.includes("/api/metrics?"))).toBe(true);
    expect(calls.some((call) => call.url.includes("/api/errors?limit=50"))).toBe(true);
    expect(calls.some((call) => call.url.includes("/api/feedback?"))).toBe(true);
    expect(calls.some((call) => call.url.includes("/api/experiments"))).toBe(true);
    expect(calls.some((call) => call.url.includes("/api/repos/stats"))).toBe(true);
    expect(calls.some((call) => call.url.includes("/api/repos/dependencies"))).toBe(true);
    expect(calls.some((call) => call.url.endsWith("/api/repos"))).toBe(true);
    expect(calls.some((call) => call.url.includes("/api/inference/stats"))).toBe(true);
    expect(calls.some((call) => call.url.includes("/api/inference/history?limit=50"))).toBe(true);
    expect(calls.some((call) => call.url.includes("/api/activity?limit=200"))).toBe(true);
    expect(calls.some((call) => call.url.includes("/api/config"))).toBe(true);

    const prsCalls = calls.filter((call) => call.url.includes("/api/prs?"));
    expect(prsCalls.length).toBe(4);
    expect(prsCalls.some((call) => call.url.includes("status=open"))).toBe(true);
    expect(prsCalls.some((call) => call.url.includes("status=merged"))).toBe(true);
    expect(prsCalls.some((call) => call.url.includes("status=closed"))).toBe(true);

    const regressionCalls = calls.filter(
      (call) => call.url.includes("/api/regressions?") && !call.url.includes("/checks")
    );
    expect(regressionCalls.length).toBe(4);
    expect(regressionCalls.some((call) => call.url.includes("status=awaiting_release"))).toBe(true);
    expect(regressionCalls.some((call) => call.url.includes("status=monitoring"))).toBe(true);
    expect(regressionCalls.some((call) => call.url.includes("status=resolved"))).toBe(true);
    expect(regressionCalls.some((call) => call.url.includes("status=regressed"))).toBe(true);

    const telemetryOverviewCalls = calls.filter((call) => call.url.includes("/api/telemetry/overview"));
    const telemetryTimeseriesCalls = calls.filter((call) => call.url.includes("/api/telemetry/timeseries"));
    const telemetryPipelineCalls = calls.filter((call) => call.url.includes("/api/telemetry/pipeline"));
    const telemetryLatencyCalls = calls.filter((call) => call.url.includes("/api/telemetry/latency"));
    expect(telemetryOverviewCalls.length).toBe(1);
    expect(telemetryTimeseriesCalls.length).toBe(4);
    expect(telemetryPipelineCalls.length).toBe(4);
    expect(telemetryLatencyCalls.length).toBe(4);
    expect(telemetryTimeseriesCalls.some((call) => call.url.includes("period=hour"))).toBe(true);
    expect(telemetryTimeseriesCalls.some((call) => call.url.includes("bucket_minutes=360"))).toBe(true);
  });
});
