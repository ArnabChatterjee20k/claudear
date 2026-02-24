import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
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

function mockLearningError() {
  globalThis.fetch = mock((url: string | URL | Request) => {
    const urlStr = typeof url === "string" ? url : url instanceof URL ? url.toString() : url.url;
    if (urlStr.includes("/api/repos") && !urlStr.includes("/learning")) {
      return Promise.resolve({
        ok: true,
        status: 200,
        statusText: "OK",
        json: () =>
          Promise.resolve([
            {
              id: 1,
              name: "my-app",
              path: "/tmp/my-app",
              scm_url: null,
              default_branch: "main",
              file_count: 10,
              last_indexed_at: "2024-01-01T00:00:00Z",
              created_at: "2024-01-01T00:00:00Z",
            },
          ]),
      });
    }
    return Promise.resolve({
      ok: false,
      status: 500,
      statusText: "Internal Server Error",
      json: () => Promise.resolve({ error: "server error" }),
      text: () => Promise.resolve('{"error":"server error"}'),
      headers: new Headers({ "content-type": "application/json" }),
    });
  }) as unknown as typeof fetch;
}

function Wrapper({ children }: { children: ReactNode }) {
  return <SWRConfig value={{ provider: () => new Map(), dedupingInterval: 0 }}>{children}</SWRConfig>;
}

function renderPage(ui: ReactNode) {
  return render(ui, { wrapper: Wrapper });
}

function setTestLocation(pathWithQuery: string) {
  window.history.replaceState(null, "", pathWithQuery);
  const url = new URL(pathWithQuery, "http://localhost");
  Object.defineProperty(window.location, "search", {
    value: url.search,
    writable: true,
    configurable: true,
  });
}

describe("LearningPage (deeper coverage)", () => {
  const originalFetch = globalThis.fetch;

  beforeEach(() => {
    setTestLocation("/learning");
  });

  afterEach(() => {
    cleanup();
    globalThis.fetch = originalFetch;
    setTestLocation("/");
  });

  test("loads repo from query param and renders all tab branches", async () => {
    setTestLocation("/learning?repo=my-app");
    mockFetchByUrl({
      "/api/repos/my-app/learning": {
        repo: "my-app",
        knowledge: [
          {
            key: "patterns",
            label: "Patterns",
            entries: [
              {
                id: 1,
                value: "Use request ids in logs",
                source_type: "review",
                confidence: 0.85,
                occurrence_count: 7,
                updated_at: "2024-01-01T00:00:00Z",
              },
              {
                id: 2,
                value: "Avoid hidden retries",
                source_type: "qa",
                confidence: 0.4,
                occurrence_count: 2,
                updated_at: "2024-01-02T00:00:00Z",
              },
            ],
          },
        ],
        knowledge_total: 2,
        instructions: [
          {
            id: 1,
            repo: "my-app",
            source_type: "qa",
            instruction_text: "Run unit tests before merge",
            occurrence_count: 4,
            confidence: 0.9,
            is_active: true,
            created_at: "2024-01-01T00:00:00Z",
            updated_at: "2024-01-01T00:00:00Z",
          },
          {
            id: 2,
            repo: "my-app",
            source_type: "qa",
            instruction_text: "Legacy instruction",
            occurrence_count: 1,
            confidence: 0.45,
            is_active: false,
            created_at: "2024-01-01T00:00:00Z",
            updated_at: "2024-01-01T00:00:00Z",
          },
        ],
        review_patterns: [
          {
            id: 1,
            scm_repo: "my-app",
            category: "missing_tests",
            pattern_text: "No tests added for bugfixes",
            example_comments: ["Need tests"],
            occurrence_count: 3,
            promoted_to_instruction: true,
            created_at: "2024-01-01T00:00:00Z",
            updated_at: "2024-01-01T00:00:00Z",
          },
          {
            id: 2,
            scm_repo: "my-app",
            category: "custom_category",
            pattern_text: "Internal convention mismatch",
            example_comments: [],
            occurrence_count: 1,
            promoted_to_instruction: false,
            created_at: "2024-01-01T00:00:00Z",
            updated_at: "2024-01-01T00:00:00Z",
          },
        ],
        review_pattern_summary: {
          total_patterns: 2,
          by_category: { missing_tests: 3, custom_category: 1 },
          promoted_count: 1,
        },
        strategies: [
          {
            id: 1,
            attempt_id: 10,
            files_explored: ["src/a.ts", "src/b.ts"],
            tests_run: 5,
            tools_used: { rg: 2 },
            fix_approach: "Reproduce then patch",
            strategy_summary: "Reproduced the issue before changing code.",
            fix_quality_score: 0.92,
            created_at: "2024-01-01T00:00:00Z",
          },
          {
            id: 2,
            attempt_id: 11,
            files_explored: ["src/c.ts"],
            tests_run: 0,
            tools_used: { rg: 1 },
            fix_approach: "Config tweak",
            strategy_summary: "Adjusted defaults to prevent edge-case failure.",
            fix_quality_score: null,
            created_at: "2024-01-02T00:00:00Z",
          },
        ],
        diff_analyses: [
          {
            id: 1,
            attempt_id: 1,
            pr_url: "https://github.com/org/repo/pull/123",
            scm_repo: "my-app",
            pr_number: 123,
            files_changed: ["src/a.ts", "src/b.ts"],
            file_types: { ts: 2 },
            change_categories: ["bugfix", "tests"],
            diff_summary: "Adds regression tests and fixes timeout handling.",
            created_at: "2024-01-01T00:00:00Z",
          },
        ],
        correlations: [
          {
            id: 1,
            repo_a: "my-app",
            repo_b: "shared-lib",
            correlation_count: 4,
            last_seen_at: "2024-01-01T00:00:00Z",
            window_hours: 24,
          },
        ],
      },
      "/api/repos": [
        {
          id: 1,
          name: "my-app",
          path: "/tmp/my-app",
          scm_url: "https://github.com/org/my-app",
          default_branch: "main",
          file_count: 10,
          last_indexed_at: "2024-01-01T00:00:00Z",
          created_at: "2024-01-01T00:00:00Z",
        },
      ],
    });

    const LearningPage = (await import("../src/pages/learning")).default;
    renderPage(<LearningPage />);

    await waitFor(() => {
      expect(screen.getByText("Knowledge Items")).toBeTruthy();
    });

    expect(screen.getByText("Patterns")).toBeTruthy();
    expect(screen.getByText("Use request ids in logs")).toBeTruthy();
    expect(screen.getByText("85%")).toBeTruthy();
    expect(screen.getByText("40%")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Instructions" }));
    await waitFor(() => {
      expect(screen.getByText("Run unit tests before merge")).toBeTruthy();
    });
    expect(screen.getByText("active")).toBeTruthy();
    expect(screen.getByText("inactive")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Review Patterns" }));
    await waitFor(() => {
      expect(screen.getByText("No tests added for bugfixes")).toBeTruthy();
    });
    expect(screen.getAllByText("missing tests").length).toBeGreaterThanOrEqual(2);
    expect(screen.getAllByText("custom category").length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText("promoted")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Strategies" }));
    await waitFor(() => {
      expect(screen.getByText("Reproduced the issue before changing code.")).toBeTruthy();
    });
    expect(screen.getByText("Quality: 92%")).toBeTruthy();
    expect(screen.getByText("Config tweak")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Diff Analyses" }));
    await waitFor(() => {
      expect(screen.getByText("#123")).toBeTruthy();
    });
    expect(screen.getByText("bugfix")).toBeTruthy();
    expect(screen.getByText("tests")).toBeTruthy();
    expect(screen.getByText("ts")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Correlations" }));
    await waitFor(() => {
      expect(screen.getByText("shared-lib")).toBeTruthy();
    });
    expect(screen.getByText("4 co-occurrences")).toBeTruthy();
    expect(screen.getByText("24h window")).toBeTruthy();
  });

  test("shows no-data message after selecting a repo", async () => {
    setTestLocation("/learning?repo=empty-repo");
    mockFetchByUrl({
      "/api/repos/empty-repo/learning": {
        repo: "empty-repo",
        knowledge: [],
        knowledge_total: 0,
        instructions: [],
        review_patterns: [],
        review_pattern_summary: { total_patterns: 0, by_category: {}, promoted_count: 0 },
        strategies: [],
        diff_analyses: [],
        correlations: [],
      },
      "/api/repos": [
        {
          id: 1,
          name: "empty-repo",
          path: "/tmp/empty-repo",
          scm_url: null,
          default_branch: "main",
          file_count: 1,
          last_indexed_at: "2024-01-01T00:00:00Z",
          created_at: "2024-01-01T00:00:00Z",
        },
      ],
    });

    const LearningPage = (await import("../src/pages/learning")).default;
    renderPage(<LearningPage />);

    await waitFor(() => {
      expect(screen.getByText(/No learning data yet for empty-repo/i)).toBeTruthy();
    });
  });

  test("shows error state when learning endpoint fails", async () => {
    setTestLocation("/learning?repo=my-app");
    mockLearningError();

    const LearningPage = (await import("../src/pages/learning")).default;
    renderPage(<LearningPage />);

    await waitFor(() => {
      expect(screen.getByText("Failed to load learning data.")).toBeTruthy();
    });
  });
});
