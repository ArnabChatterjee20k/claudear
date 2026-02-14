import { describe, expect, test, mock, afterEach } from "bun:test";
import {
  fetchOverview,
  getStats,
  fetchAttempts,
  getHealth,
  type Overview,
  type Stats,
  type AttemptsResponse,
  type Health,
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

describe("api", () => {
  const originalFetch = globalThis.fetch;

  afterEach(() => {
    globalThis.fetch = originalFetch;
  });

  describe("fetchOverview", () => {
    test("fetches overview data successfully", async () => {
      const mockOverview: Overview = {
        stats: {
          total: 100,
          pending: 5,
          success: 50,
          failed: 10,
          merged: 30,
          closed: 3,
          cannot_fix: 2,
          by_source: {},
        },
        success_rate: 80,
        merge_rate: 30,
        recent_attempts: [],
        sources: [],
      };

      mockFetch(mockOverview);

      const result = await fetchOverview();
      expect(result).toEqual(mockOverview);
      expect(fetch).toHaveBeenCalledWith("/api/stats/overview");
    });

    test("throws on non-ok response", async () => {
      mockFetch(null, false);
      expect(fetchOverview()).rejects.toThrow();
    });
  });

  describe("getStats", () => {
    test("fetches stats successfully", async () => {
      const mockStats: Stats = {
        total: 100,
        pending: 5,
        success: 50,
        failed: 10,
        merged: 30,
        closed: 3,
        cannot_fix: 2,
        by_source: {},
      };

      mockFetch(mockStats);

      const result = await getStats();
      expect(result).toEqual(mockStats);
      expect(fetch).toHaveBeenCalledWith("/api/stats");
    });

    test("throws on error", async () => {
      mockFetch(null, false);
      expect(getStats()).rejects.toThrow();
    });
  });

  describe("fetchAttempts", () => {
    test("fetches attempts without params", async () => {
      const mockResponse: AttemptsResponse = {
        attempts: [],
        total: 0,
        page: 1,
        per_page: 20,
      };

      mockFetch(mockResponse);

      const result = await fetchAttempts();
      expect(result).toEqual(mockResponse);
      expect(fetch).toHaveBeenCalledWith("/api/attempts?");
    });

    test("fetches attempts with status filter", async () => {
      mockFetch({ attempts: [], total: 0, page: 1, per_page: 20 });
      await fetchAttempts({ status: "pending" });
      expect(fetch).toHaveBeenCalledWith("/api/attempts?status=pending");
    });

    test("fetches attempts with source filter", async () => {
      mockFetch({ attempts: [], total: 0, page: 1, per_page: 20 });
      await fetchAttempts({ source: "linear" });
      expect(fetch).toHaveBeenCalledWith("/api/attempts?source=linear");
    });

    test("fetches attempts with pagination", async () => {
      mockFetch({ attempts: [], total: 100, page: 2, per_page: 10 });
      await fetchAttempts({ page: 2, per_page: 10 });
      expect(fetch).toHaveBeenCalledWith("/api/attempts?page=2&per_page=10");
    });

    test("fetches attempts with all params", async () => {
      mockFetch({ attempts: [], total: 50, page: 1, per_page: 25 });
      await fetchAttempts({
        status: "success",
        source: "sentry",
        page: 1,
        per_page: 25,
      });
      expect(fetch).toHaveBeenCalledWith(
        "/api/attempts?status=success&source=sentry&page=1&per_page=25"
      );
    });

    test("throws on error", async () => {
      mockFetch(null, false);
      expect(fetchAttempts()).rejects.toThrow();
    });
  });

  describe("getHealth", () => {
    test("fetches health status", async () => {
      const mockHealth: Health = {
        status: "ok",
        version: "1.0.0",
        uptime_secs: 3600,
        database: { status: "ok" },
      };

      mockFetch(mockHealth);

      const result = await getHealth();
      expect(result).toEqual(mockHealth);
      expect(fetch).toHaveBeenCalledWith("/api/health");
    });

    test("throws on error", async () => {
      mockFetch(null, false);
      expect(getHealth()).rejects.toThrow();
    });
  });
});
