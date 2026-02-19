import { describe, test, expect, afterEach, mock } from "bun:test";
import { render, screen, waitFor, cleanup } from "@testing-library/react";
import { SWRConfig } from "swr";
import { HealthBar } from "../src/components/layout/health-bar";

function mockFetchHealth(data: unknown) {
  globalThis.fetch = mock(() =>
    Promise.resolve({
      ok: true,
      json: () => Promise.resolve(data),
    })
  ) as unknown as typeof fetch;
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
  globalThis.fetch = undefined as unknown as typeof fetch;
});

describe("HealthBar", () => {
  test("returns null when health data not loaded yet", () => {
    // Fetch that never resolves
    globalThis.fetch = mock(() => new Promise<Response>(() => {})) as unknown as typeof fetch;
    const { container } = renderWithSWR(<HealthBar />);
    expect(container.innerHTML).toBe("");
  });

  test("shows Healthy for ok status", async () => {
    mockFetchHealth({
      status: "ok",
      version: "1.2.3",
      uptime_secs: 3600,
      database: { status: "ok" },
    });

    renderWithSWR(<HealthBar />);

    await waitFor(() => {
      expect(screen.getByText("Healthy")).toBeTruthy();
    });
  });

  test("shows Degraded when status is not ok", async () => {
    mockFetchHealth({
      status: "degraded",
      version: "1.2.3",
      uptime_secs: 100,
      database: { status: "ok" },
    });

    renderWithSWR(<HealthBar />);

    await waitFor(() => {
      expect(screen.getByText("Degraded")).toBeTruthy();
    });
  });

  test("shows Healthy when database status is ok even with error field", async () => {
    mockFetchHealth({
      status: "ok",
      version: "1.2.3",
      uptime_secs: 100,
      database: { status: "error", error: "connection refused" },
    });

    renderWithSWR(<HealthBar />);

    await waitFor(() => {
      expect(screen.getByText("Healthy")).toBeTruthy();
    });
  });

  test("displays version string", async () => {
    mockFetchHealth({
      status: "ok",
      version: "1.2.3",
      uptime_secs: 3600,
      database: { status: "ok" },
    });

    renderWithSWR(<HealthBar />);

    await waitFor(() => {
      expect(screen.getByText("v1.2.3")).toBeTruthy();
    });
  });

  test("formatUptime: seconds", async () => {
    mockFetchHealth({
      status: "ok",
      version: "1.0.0",
      uptime_secs: 45,
      database: { status: "ok" },
    });

    renderWithSWR(<HealthBar />);

    await waitFor(() => {
      expect(screen.getByText("Uptime: 45s")).toBeTruthy();
    });
  });

  test("formatUptime: minutes", async () => {
    mockFetchHealth({
      status: "ok",
      version: "1.0.0",
      uptime_secs: 300,
      database: { status: "ok" },
    });

    renderWithSWR(<HealthBar />);

    await waitFor(() => {
      expect(screen.getByText("Uptime: 5m")).toBeTruthy();
    });
  });

  test("formatUptime: hours and minutes", async () => {
    mockFetchHealth({
      status: "ok",
      version: "1.0.0",
      uptime_secs: 7260,
      database: { status: "ok" },
    });

    renderWithSWR(<HealthBar />);

    await waitFor(() => {
      expect(screen.getByText("Uptime: 2h 1m")).toBeTruthy();
    });
  });

  test("formatUptime: days and hours", async () => {
    mockFetchHealth({
      status: "ok",
      version: "1.0.0",
      uptime_secs: 90000,
      database: { status: "ok" },
    });

    renderWithSWR(<HealthBar />);

    await waitFor(() => {
      expect(screen.getByText("Uptime: 1d 1h")).toBeTruthy();
    });
  });
});
