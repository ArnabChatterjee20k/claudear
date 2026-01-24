import { describe, expect, test, mock, beforeEach, afterEach } from "bun:test";
import {
  fetchOverview,
  fetchStats,
  fetchAttempts,
  fetchHealth,
  type Overview,
  type Stats,
  type AttemptsResponse,
} from "../src/lib/api";

describe("api", () => {
  const originalFetch = globalThis.fetch;

  beforeEach(() => {
    // Reset fetch mock before each test
  });

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

      globalThis.fetch = mock(() =>
        Promise.resolve({
          ok: true,
          json: () => Promise.resolve(mockOverview),
        } as Response)
      );

      const result = await fetchOverview();
      expect(result).toEqual(mockOverview);
      expect(fetch).toHaveBeenCalledWith("/api/stats/overview");
    });

    test("throws on non-ok response", async () => {
      globalThis.fetch = mock(() =>
        Promise.resolve({
          ok: false,
        } as Response)
      );

      expect(fetchOverview()).rejects.toThrow("Failed to fetch overview");
    });
  });

  describe("fetchStats", () => {
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

      globalThis.fetch = mock(() =>
        Promise.resolve({
          ok: true,
          json: () => Promise.resolve(mockStats),
        } as Response)
      );

      const result = await fetchStats();
      expect(result).toEqual(mockStats);
      expect(fetch).toHaveBeenCalledWith("/api/stats");
    });

    test("throws on error", async () => {
      globalThis.fetch = mock(() =>
        Promise.resolve({
          ok: false,
        } as Response)
      );

      expect(fetchStats()).rejects.toThrow("Failed to fetch stats");
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

      globalThis.fetch = mock(() =>
        Promise.resolve({
          ok: true,
          json: () => Promise.resolve(mockResponse),
        } as Response)
      );

      const result = await fetchAttempts();
      expect(result).toEqual(mockResponse);
      expect(fetch).toHaveBeenCalledWith("/api/attempts?");
    });

    test("fetches attempts with status filter", async () => {
      const mockResponse: AttemptsResponse = {
        attempts: [],
        total: 0,
        page: 1,
        per_page: 20,
      };

      globalThis.fetch = mock(() =>
        Promise.resolve({
          ok: true,
          json: () => Promise.resolve(mockResponse),
        } as Response)
      );

      await fetchAttempts({ status: "pending" });
      expect(fetch).toHaveBeenCalledWith("/api/attempts?status=pending");
    });

    test("fetches attempts with source filter", async () => {
      const mockResponse: AttemptsResponse = {
        attempts: [],
        total: 0,
        page: 1,
        per_page: 20,
      };

      globalThis.fetch = mock(() =>
        Promise.resolve({
          ok: true,
          json: () => Promise.resolve(mockResponse),
        } as Response)
      );

      await fetchAttempts({ source: "linear" });
      expect(fetch).toHaveBeenCalledWith("/api/attempts?source=linear");
    });

    test("fetches attempts with pagination", async () => {
      const mockResponse: AttemptsResponse = {
        attempts: [],
        total: 100,
        page: 2,
        per_page: 10,
      };

      globalThis.fetch = mock(() =>
        Promise.resolve({
          ok: true,
          json: () => Promise.resolve(mockResponse),
        } as Response)
      );

      await fetchAttempts({ page: 2, per_page: 10 });
      expect(fetch).toHaveBeenCalledWith("/api/attempts?page=2&per_page=10");
    });

    test("fetches attempts with all params", async () => {
      const mockResponse: AttemptsResponse = {
        attempts: [],
        total: 50,
        page: 1,
        per_page: 25,
      };

      globalThis.fetch = mock(() =>
        Promise.resolve({
          ok: true,
          json: () => Promise.resolve(mockResponse),
        } as Response)
      );

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
      globalThis.fetch = mock(() =>
        Promise.resolve({
          ok: false,
        } as Response)
      );

      expect(fetchAttempts()).rejects.toThrow("Failed to fetch attempts");
    });
  });

  describe("fetchHealth", () => {
    test("fetches health status", async () => {
      const mockHealth = { status: "ok", version: "1.0.0" };

      globalThis.fetch = mock(() =>
        Promise.resolve({
          ok: true,
          json: () => Promise.resolve(mockHealth),
        } as Response)
      );

      const result = await fetchHealth();
      expect(result).toEqual(mockHealth);
      expect(fetch).toHaveBeenCalledWith("/api/health");
    });

    test("throws on error", async () => {
      globalThis.fetch = mock(() =>
        Promise.resolve({
          ok: false,
        } as Response)
      );

      expect(fetchHealth()).rejects.toThrow("Failed to fetch health");
    });
  });
});
