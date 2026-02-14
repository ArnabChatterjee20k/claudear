import { describe, test, expect, afterEach, mock } from "bun:test";
import { render, screen, waitFor, cleanup } from "@testing-library/react";
import { SWRConfig } from "swr";
import OverviewPage from "../src/pages/overview";

const mockOverview = {
  stats: {
    total: 100,
    pending: 5,
    success: 50,
    failed: 10,
    merged: 30,
    closed: 3,
    cannot_fix: 2,
    by_source: {
      sentry: { total: 60, success: 30, failed: 5, merged: 20, closed: 2, cannot_fix: 3 },
      linear: { total: 40, success: 20, failed: 5, merged: 10, closed: 1, cannot_fix: 4 },
    },
  },
  success_rate: 80.0,
  merge_rate: 30.0,
  recent_attempts: [
    {
      id: 1,
      source: "sentry",
      short_id: "abc123",
      title: "Fix timeout",
      status: "success",
      pr_url: "https://github.com/test/pr/1",
      attempted_at: "2024-01-01T00:00:00Z",
      retry_count: 0,
    },
    {
      id: 2,
      source: "linear",
      short_id: "def456",
      title: "Fix auth",
      status: "failed",
      pr_url: null,
      attempted_at: "2024-01-01T00:00:00Z",
      retry_count: 2,
    },
  ],
  sources: [
    { name: "sentry", total: 60, success: 30, failed: 5, merged: 20, success_rate: 83.3 },
    { name: "linear", total: 40, success: 20, failed: 5, merged: 10, success_rate: 75.0 },
  ],
};

const mockRetries = {
  retryable: [
    {
      id: 3,
      source: "sentry",
      short_id: "ghi789",
      title: "Fix crash",
      status: "failed",
      pr_url: null,
      attempted_at: "2024-01-01T00:00:00Z",
      retry_count: 1,
    },
  ],
  ready: [
    {
      id: 3,
      source: "sentry",
      short_id: "ghi789",
      title: "Fix crash",
      status: "failed",
      pr_url: null,
      attempted_at: "2024-01-01T00:00:00Z",
      retry_count: 1,
    },
  ],
  max_retries: 3,
};

const emptyRetries = { retryable: [], ready: [], max_retries: 3 };

function mockFetchByUrl(urlMap: Record<string, unknown>) {
  return mock((url: string) => {
    for (const [pattern, data] of Object.entries(urlMap)) {
      if (url.includes(pattern)) {
        return Promise.resolve({
          ok: true,
          json: () => Promise.resolve(data),
        });
      }
    }
    return Promise.resolve({
      ok: false,
      status: 404,
      json: () => Promise.resolve({}),
    });
  });
}

function renderWithSWR(ui: React.ReactElement) {
  return render(
    <SWRConfig value={{ provider: () => new Map(), dedupingInterval: 0 }}>
      {ui}
    </SWRConfig>
  );
}

afterEach(() => {
  cleanup();
  globalThis.fetch = undefined as unknown as unknown as typeof fetch;
});

describe("OverviewPage", () => {
  test("renders loading state", () => {
    // Fetch that never resolves
    globalThis.fetch = mock(() => new Promise<Response>(() => {})) as unknown as typeof fetch;
    renderWithSWR(<OverviewPage />);
    expect(screen.getByText("Loading...")).toBeTruthy();
  });

  test("renders error state", async () => {
    globalThis.fetch = mock(() =>
      Promise.resolve({ ok: false, status: 500, json: () => Promise.resolve({}) })
    ) as unknown as typeof fetch;

    renderWithSWR(<OverviewPage />);

    await waitFor(() => {
      expect(screen.getByText("Error")).toBeTruthy();
    });
    expect(screen.getByText(/Failed to connect to the API/)).toBeTruthy();
  });

  test("renders stats cards with data", async () => {
    globalThis.fetch = mockFetchByUrl({
      "/api/stats/overview": mockOverview,
      "/api/retries": emptyRetries,
    }) as unknown as typeof fetch;

    renderWithSWR(<OverviewPage />);

    await waitFor(() => {
      expect(screen.getByText("Total Attempts")).toBeTruthy();
    });
    expect(screen.getByText("100")).toBeTruthy();
    expect(screen.getByText("80.0%")).toBeTruthy();
    expect(screen.getByText("30.0%")).toBeTruthy();
  });

  test("renders status cards", async () => {
    globalThis.fetch = mockFetchByUrl({
      "/api/stats/overview": mockOverview,
      "/api/retries": emptyRetries,
    }) as unknown as typeof fetch;

    renderWithSWR(<OverviewPage />);

    await waitFor(() => {
      expect(screen.getAllByText("pending").length).toBeGreaterThan(0);
    });
    expect(screen.getAllByText("success").length).toBeGreaterThan(0);
    expect(screen.getAllByText("merged").length).toBeGreaterThan(0);
    expect(screen.getAllByText("closed").length).toBeGreaterThan(0);
    expect(screen.getAllByText("failed").length).toBeGreaterThan(0);
    expect(screen.getByText("cannot fix")).toBeTruthy();
  });

  test("renders recent attempts", async () => {
    globalThis.fetch = mockFetchByUrl({
      "/api/stats/overview": mockOverview,
      "/api/retries": emptyRetries,
    }) as unknown as typeof fetch;

    renderWithSWR(<OverviewPage />);

    await waitFor(() => {
      expect(screen.getByText("abc123")).toBeTruthy();
    });
    expect(screen.getByText("def456")).toBeTruthy();
  });

  test("renders sources list", async () => {
    globalThis.fetch = mockFetchByUrl({
      "/api/stats/overview": mockOverview,
      "/api/retries": emptyRetries,
    }) as unknown as typeof fetch;

    renderWithSWR(<OverviewPage />);

    await waitFor(() => {
      expect(screen.getByText("Sources")).toBeTruthy();
    });

    // Source names are rendered capitalized via CSS, but the textContent is lowercase
    const sentryEls = screen.getAllByText("sentry");
    expect(sentryEls.length).toBeGreaterThan(0);
    const linearEls = screen.getAllByText("linear");
    expect(linearEls.length).toBeGreaterThan(0);
    expect(screen.getByText("83.3%")).toBeTruthy();
  });

  test("renders source breakdown table", async () => {
    globalThis.fetch = mockFetchByUrl({
      "/api/stats/overview": mockOverview,
      "/api/retries": emptyRetries,
    }) as unknown as typeof fetch;

    renderWithSWR(<OverviewPage />);

    await waitFor(() => {
      expect(screen.getByText("Source Breakdown")).toBeTruthy();
    });
    // Table cells for sentry row
    expect(screen.getByText("60")).toBeTruthy();
    // Table cells for linear row
    expect(screen.getByText("40")).toBeTruthy();
  });

  test("renders retries section", async () => {
    globalThis.fetch = mockFetchByUrl({
      "/api/stats/overview": mockOverview,
      "/api/retries": mockRetries,
    }) as unknown as typeof fetch;

    renderWithSWR(<OverviewPage />);

    await waitFor(() => {
      expect(screen.getByText("Ready for Retry")).toBeTruthy();
    });
    expect(screen.getByText("ghi789")).toBeTruthy();
  });

  test("does not render retries section when empty", async () => {
    globalThis.fetch = mockFetchByUrl({
      "/api/stats/overview": mockOverview,
      "/api/retries": emptyRetries,
    }) as unknown as typeof fetch;

    renderWithSWR(<OverviewPage />);

    await waitFor(() => {
      expect(screen.getByText("Total Attempts")).toBeTruthy();
    });
    expect(screen.queryByText("Ready for Retry")).toBeNull();
  });
});
