import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { SWRConfig } from "swr";
import type { ReactNode } from "react";
import { RouterProvider } from "../src/router";

function mockFetchByUrl(urlMap: Record<string, unknown>) {
  const sortedPatterns = Object.keys(urlMap).sort((a, b) => b.length - a.length);
  globalThis.fetch = mock((url: string | URL | Request) => {
    const urlStr = typeof url === "string" ? url : url instanceof URL ? url.toString() : url.url;
    for (const pattern of sortedPatterns) {
      if (urlStr.includes(pattern)) {
        const data = urlMap[pattern];
        return Promise.resolve({
          ok: true,
          status: 200,
          statusText: "OK",
          json: () => Promise.resolve(data),
          text: () => Promise.resolve(JSON.stringify(data)),
          headers: new Headers({ "content-type": "application/json" }),
        });
      }
    }
    return Promise.resolve({
      ok: true,
      status: 200,
      statusText: "OK",
      json: () => Promise.resolve([]),
      text: () => Promise.resolve("[]"),
      headers: new Headers({ "content-type": "application/json" }),
    });
  }) as unknown as typeof fetch;
}

class MockWebSocket {
  static instances: MockWebSocket[] = [];
  static OPEN = 1;
  url: string;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  readyState = MockWebSocket.OPEN;

  constructor(url: string) {
    this.url = url;
    MockWebSocket.instances.push(this);
  }

  close() {
    this.onclose?.();
  }

  emitMessage(data: unknown) {
    this.onmessage?.({ data: typeof data === "string" ? data : JSON.stringify(data) });
  }

  emitError() {
    this.onerror?.();
  }
}

function Wrapper({ children }: { children: ReactNode }) {
  const provider = () => {
    const cache = new Map();
    cache.set("repos", []);
    cache.set("repo-stats", { repo_count: 0, file_count: 0, last_indexed_at: null });
    cache.set("dependencies", []);
    return cache;
  };
  return (
    <RouterProvider>
      <SWRConfig value={{ provider, dedupingInterval: 0, revalidateOnMount: true }}>
        {children}
      </SWRConfig>
    </RouterProvider>
  );
}

describe("ReposPage interactions", () => {
  const originalFetch = globalThis.fetch;
  const OriginalWebSocket = globalThis.WebSocket;

  beforeEach(() => {
    MockWebSocket.instances = [];
    globalThis.WebSocket = MockWebSocket as unknown as typeof WebSocket;
    window.history.replaceState(null, "", "/repos");
  });

  afterEach(() => {
    cleanup();
    globalThis.fetch = originalFetch;
    globalThis.WebSocket = OriginalWebSocket;
    window.history.replaceState(null, "", "/");
  });

  test("renders indexing progress, dependency tab, repo modal, and learning navigation", async () => {
    mockFetchByUrl({
      "/api/repos/stats": {
        repo_count: 2,
        file_count: 12345,
        last_indexed_at: null,
      },
      "/api/repos/dependencies": [
        { id: 1, upstream: "api-service", downstream: "shared-lib", dep_type: "npm" },
      ],
      "/api/repos": [
        {
          id: 1,
          name: "my repo",
          path: "/tmp/my-repo",
          scm_url: null,
          default_branch: "main",
          file_count: 2500,
          last_indexed_at: "2024-01-05T00:00:00Z",
          created_at: "2024-01-01T00:00:00Z",
        },
        {
          id: 2,
          name: "shared-lib",
          path: "/tmp/shared-lib",
          scm_url: "https://github.com/org/shared-lib",
          default_branch: "main",
          file_count: 100,
          last_indexed_at: "2024-01-06T00:00:00Z",
          created_at: "2024-01-02T00:00:00Z",
        },
      ],
    });

    const ReposPage = (await import("../src/pages/repos")).default;
    render(<ReposPage />, { wrapper: Wrapper });

    await waitFor(() => {
      expect(screen.getByRole("heading", { name: "Repositories" })).toBeTruthy();
      expect(screen.getByText("my repo")).toBeTruthy();
    });

    expect(MockWebSocket.instances.length).toBeGreaterThanOrEqual(1);
    const ws = MockWebSocket.instances[0];
    expect(ws.url).toContain("/api/repos/indexing-progress");
    await waitFor(() => {
      expect(ws.onmessage).toBeTruthy();
    });

    act(() => {
      ws.emitMessage("not-json");
      ws.emitMessage({
        status: "running",
        total_repos: 2,
        indexed_repos: 1,
        current_repo: "my repo",
        current_repo_files: 123,
        total_files_indexed: 1000,
        started_at: "2024-01-01T00:00:00Z",
        updated_at: "2024-01-01T00:01:00Z",
      });
    });

    await waitFor(() => {
      expect(screen.getByText("Indexing in progress")).toBeTruthy();
    });
    expect(screen.getByText("1 / 2 repos (50%)")).toBeTruthy();
    expect(screen.getAllByText("my repo").length).toBeGreaterThanOrEqual(1);
    expect(screen.getByText("(123 files)")).toBeTruthy();
    expect(screen.getByText("1.0k files indexed so far")).toBeTruthy();

    const originalSetTimeout = globalThis.setTimeout;
    let reconnectScheduled = false;
    globalThis.setTimeout = ((_handler: TimerHandler, _timeout?: number) => {
      reconnectScheduled = true;
      return 1 as unknown as ReturnType<typeof setTimeout>;
    }) as unknown as typeof setTimeout;
    act(() => {
      ws.onclose?.();
    });
    globalThis.setTimeout = originalSetTimeout;
    expect(reconnectScheduled).toBe(true);

    fireEvent.click(screen.getByRole("button", { name: "Dependencies" }));
    await waitFor(() => {
      expect(screen.getByText("api-service")).toBeTruthy();
      expect(screen.getByText("shared-lib")).toBeTruthy();
    });

    fireEvent.click(screen.getByRole("button", { name: "Repositories" }));
    await waitFor(() => {
      expect(screen.getAllByText("my repo").length).toBeGreaterThanOrEqual(1);
    });

    fireEvent.click(screen.getByText("/tmp/shared-lib"));
    await waitFor(() => {
      expect(screen.getByText("View Learning")).toBeTruthy();
    });
    expect(screen.getByText("https://github.com/org/shared-lib")).toBeTruthy();
    fireEvent.keyDown(document, { key: "Escape" });

    await waitFor(() => {
      expect(screen.queryByText("https://github.com/org/shared-lib")).toBeNull();
    });

    fireEvent.click(screen.getByText("/tmp/my-repo"));

    await waitFor(() => {
      expect(screen.getByText("View Learning")).toBeTruthy();
    });
    expect(screen.getAllByText("/tmp/my-repo").length).toBeGreaterThanOrEqual(1);
    expect(screen.getAllByText("Default Branch").length).toBeGreaterThanOrEqual(1);
    expect(screen.getAllByText("--").length).toBeGreaterThanOrEqual(1);

    fireEvent.click(screen.getByRole("button", { name: "View Learning" }));

    await waitFor(() => {
      expect(window.location.pathname).toBe("/learning");
    });
    expect(window.location.href).toContain("/learning?repo=my%20repo");
  });
});
