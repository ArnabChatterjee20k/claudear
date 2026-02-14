import { describe, expect, test, mock, afterEach } from "bun:test";
import {
  fetchActivity,
  fetchAttemptDetail,
  fetchAnalyticsSummary,
  fetchMetrics,
  fetchErrors,
  fetchPrs,
  fetchPrAnalytics,
  fetchFeedback,
  fetchRegressions,
  fetchRegressionChecks,
  fetchExperiments,
  fetchRepos,
  fetchRepoStats,
  fetchDependencies,
  fetchInferenceStats,
  fetchInferenceHistory,
  getRetries,
  getSources,
} from "../src/lib/api";

function mockFetch(data: unknown, ok = true) {
  globalThis.fetch = mock(() =>
    Promise.resolve({
      ok,
      status: ok ? 200 : 500,
      json: () => Promise.resolve(data),
    })
  ) as unknown as typeof fetch;
}

describe("new api endpoints", () => {
  const originalFetch = globalThis.fetch;
  afterEach(() => {
    globalThis.fetch = originalFetch;
  });

  test("fetchActivity calls correct URL", async () => {
    mockFetch([]);
    await fetchActivity({ limit: 25, source: "linear" });
    expect(fetch).toHaveBeenCalledWith("/api/activity?limit=25&source=linear");
  });

  test("fetchActivity with defaults", async () => {
    mockFetch([]);
    await fetchActivity();
    expect(fetch).toHaveBeenCalledWith("/api/activity?");
  });

  test("fetchAttemptDetail calls correct URL", async () => {
    mockFetch({ attempt: {}, executions: [], reviews: [], feedback: null });
    await fetchAttemptDetail(42);
    expect(fetch).toHaveBeenCalledWith("/api/attempts/42/detail");
  });

  test("fetchAnalyticsSummary calls correct URL", async () => {
    mockFetch({});
    await fetchAnalyticsSummary();
    expect(fetch).toHaveBeenCalledWith("/api/analytics/summary");
  });

  test("fetchMetrics calls correct URL with params", async () => {
    mockFetch([]);
    await fetchMetrics({ name: "processing_time", period: "week", limit: 100 });
    expect(fetch).toHaveBeenCalledWith(
      "/api/metrics?name=processing_time&period=week&limit=100"
    );
  });

  test("fetchErrors calls correct URL", async () => {
    mockFetch([]);
    await fetchErrors(25);
    expect(fetch).toHaveBeenCalledWith("/api/errors?limit=25");
  });

  test("fetchErrors uses default limit", async () => {
    mockFetch([]);
    await fetchErrors();
    expect(fetch).toHaveBeenCalledWith("/api/errors?limit=50");
  });

  test("fetchPrs calls correct URL", async () => {
    mockFetch([]);
    await fetchPrs({ status: "merged", limit: 50 });
    expect(fetch).toHaveBeenCalledWith("/api/prs?status=merged&limit=50");
  });

  test("fetchPrAnalytics calls correct URL", async () => {
    mockFetch({});
    await fetchPrAnalytics();
    expect(fetch).toHaveBeenCalledWith("/api/prs/analytics");
  });

  test("fetchFeedback calls correct URL", async () => {
    mockFetch([]);
    await fetchFeedback({ source: "sentry", limit: 20 });
    expect(fetch).toHaveBeenCalledWith("/api/feedback?source=sentry&limit=20");
  });

  test("fetchRegressions calls correct URL", async () => {
    mockFetch([]);
    await fetchRegressions("monitoring");
    expect(fetch).toHaveBeenCalledWith("/api/regressions?status=monitoring");
  });

  test("fetchRegressions without status", async () => {
    mockFetch([]);
    await fetchRegressions();
    expect(fetch).toHaveBeenCalledWith("/api/regressions?");
  });

  test("fetchRegressionChecks calls correct URL", async () => {
    mockFetch([]);
    await fetchRegressionChecks(5);
    expect(fetch).toHaveBeenCalledWith("/api/regressions/5/checks");
  });

  test("fetchExperiments calls correct URL", async () => {
    mockFetch([]);
    await fetchExperiments();
    expect(fetch).toHaveBeenCalledWith("/api/experiments");
  });

  test("fetchRepos calls correct URL", async () => {
    mockFetch([]);
    await fetchRepos();
    expect(fetch).toHaveBeenCalledWith("/api/repos");
  });

  test("fetchRepoStats calls correct URL", async () => {
    mockFetch({});
    await fetchRepoStats();
    expect(fetch).toHaveBeenCalledWith("/api/repos/stats");
  });

  test("fetchDependencies calls correct URL", async () => {
    mockFetch([]);
    await fetchDependencies();
    expect(fetch).toHaveBeenCalledWith("/api/repos/dependencies");
  });

  test("fetchInferenceStats calls correct URL", async () => {
    mockFetch({});
    await fetchInferenceStats();
    expect(fetch).toHaveBeenCalledWith("/api/inference/stats");
  });

  test("fetchInferenceHistory calls correct URL", async () => {
    mockFetch([]);
    await fetchInferenceHistory(25);
    expect(fetch).toHaveBeenCalledWith("/api/inference/history?limit=25");
  });

  test("fetchInferenceHistory uses default limit", async () => {
    mockFetch([]);
    await fetchInferenceHistory();
    expect(fetch).toHaveBeenCalledWith("/api/inference/history?limit=50");
  });

  test("getRetries calls correct URL", async () => {
    mockFetch({ retryable: [], ready: [], max_retries: 3 });
    await getRetries();
    expect(fetch).toHaveBeenCalledWith("/api/retries");
  });

  test("getSources calls correct URL", async () => {
    mockFetch({ sources: [] });
    await getSources();
    expect(fetch).toHaveBeenCalledWith("/api/sources");
  });

  // Error handling tests
  test("all fetchers throw on error response", async () => {
    mockFetch(null, false);
    expect(fetchActivity()).rejects.toThrow();
    expect(fetchAnalyticsSummary()).rejects.toThrow();
    expect(fetchErrors()).rejects.toThrow();
    expect(fetchPrs()).rejects.toThrow();
    expect(fetchPrAnalytics()).rejects.toThrow();
    expect(fetchFeedback()).rejects.toThrow();
    expect(fetchRegressions()).rejects.toThrow();
    expect(fetchExperiments()).rejects.toThrow();
    expect(fetchRepos()).rejects.toThrow();
    expect(fetchRepoStats()).rejects.toThrow();
    expect(fetchDependencies()).rejects.toThrow();
    expect(fetchInferenceStats()).rejects.toThrow();
    expect(fetchInferenceHistory()).rejects.toThrow();
  });
});
