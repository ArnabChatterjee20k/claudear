import { afterEach, describe, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { SWRConfig } from "swr";
import type { ReactNode } from "react";

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

function Wrapper({ children }: { children: ReactNode }) {
  return (
    <SWRConfig value={{ provider: () => new Map(), dedupingInterval: 0, revalidateOnMount: true }}>
      {children}
    </SWRConfig>
  );
}

describe("InferencePage interactions", () => {
  const originalFetch = globalThis.fetch;

  afterEach(() => {
    cleanup();
    globalThis.fetch = originalFetch;
  });

  test("renders table branches and modal detail variants for inference history entries", async () => {
    mockFetchByUrl({
      "/api/inference/stats": {
        total_attempts: 3,
        with_feedback: 3,
        correct: 1,
        accuracy: 33.3,
        by_confidence: { high: 1, medium: 0, low: 1, none: 1 },
      },
      "/api/inference/history": [
        {
          id: 1,
          issue_id: "INF-TRUE",
          issue_source: "linear",
          extracted_keywords: "alpha, , beta",
          inferred_repo_name: "repo-a",
          confidence: null,
          inference_reason: "keyword match",
          was_correct: true,
          duration_ms: 42,
          created_at: "2024-01-01T00:00:00Z",
        },
        {
          id: 2,
          issue_id: "INF-FALSE",
          issue_source: "github",
          extracted_keywords: null,
          inferred_repo_name: "repo-b",
          confidence: "custom",
          inference_reason: "manual override",
          was_correct: false,
          duration_ms: null,
          created_at: "2024-01-02T00:00:00Z",
        },
        {
          id: 3,
          issue_id: "INF-UNKNOWN",
          issue_source: "linear",
          extracted_keywords: null,
          inferred_repo_name: null,
          confidence: null,
          inference_reason: null,
          was_correct: null,
          duration_ms: null,
          created_at: "2024-01-03T00:00:00Z",
        },
      ],
    });

    const InferencePage = (await import("../src/pages/inference")).default;
    render(<InferencePage />, { wrapper: Wrapper });

    await waitFor(() => {
      expect(screen.getByText("INF-TRUE")).toBeTruthy();
      expect(screen.getByText("INF-FALSE")).toBeTruthy();
      expect(screen.getByText("INF-UNKNOWN")).toBeTruthy();
    });

    expect(screen.getByText("33.3%")).toBeTruthy();
    expect(document.querySelector(".lucide-x")).toBeTruthy();

    fireEvent.click(screen.getByText("INF-FALSE"));
    await waitFor(() => {
      expect(screen.getByText("Inference: INF-FALSE")).toBeTruthy();
    });
    expect(screen.getAllByText("custom").length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText("No")).toBeTruthy();
    expect(screen.getAllByText("--").length).toBeGreaterThanOrEqual(1);
    expect(screen.getByText("manual override")).toBeTruthy();
    expect(screen.queryByText("Keywords")).toBeNull();

    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => {
      expect(screen.queryByText("Inference: INF-FALSE")).toBeNull();
    });

    fireEvent.click(screen.getByText("INF-TRUE"));
    await waitFor(() => {
      expect(screen.getByText("Inference: INF-TRUE")).toBeTruthy();
    });
    expect(screen.getByText("Yes")).toBeTruthy();
    expect(screen.getAllByText("42ms").length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText("keyword match")).toBeTruthy();
    expect(screen.getByText("Keywords")).toBeTruthy();
    expect(screen.getByText("alpha")).toBeTruthy();
    expect(screen.getByText("beta")).toBeTruthy();

    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => {
      expect(screen.queryByText("Inference: INF-TRUE")).toBeNull();
    });

    fireEvent.click(screen.getByText("INF-UNKNOWN"));
    await waitFor(() => {
      expect(screen.getByText("Inference: INF-UNKNOWN")).toBeTruthy();
    });
    expect(screen.queryByText("Inference Reason")).toBeNull();
    expect(screen.queryByText("Keywords")).toBeNull();
  });
});
