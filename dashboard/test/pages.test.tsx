import { describe, test, expect, mock, afterEach, beforeEach } from "bun:test";
import { render, screen, waitFor, cleanup, fireEvent } from "@testing-library/react";
import { SWRConfig } from "swr";
import type { ReactNode } from "react";

// ─── Helpers ──────────────────────────────────

const originalFetch = globalThis.fetch;

function mockFetch(data: unknown, ok = true) {
  globalThis.fetch = mock(() =>
    Promise.resolve({
      ok,
      status: ok ? 200 : 500,
      statusText: ok ? "OK" : "Internal Server Error",
      json: () => Promise.resolve(data),
      text: () => Promise.resolve(JSON.stringify(data)),
      headers: new Headers({ "content-type": "application/json" }),
    })
  ) as unknown as typeof fetch;
}

/** Mock fetch that returns different data based on URL matching.
 *  Patterns are sorted by length (longest first) so more specific URLs match before shorter ones. */
function mockFetchByUrl(urlMap: Record<string, unknown>) {
  // Sort patterns by length descending so "/api/repos/stats" matches before "/api/repos"
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
    // Fallback: return empty array (safe for both array and object consumers)
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

function mockFetchError() {
  globalThis.fetch = mock(() =>
    Promise.resolve({
      ok: false,
      status: 500,
      statusText: "Internal Server Error",
      json: () => Promise.resolve({ error: "server error" }),
      text: () => Promise.resolve('{"error":"server error"}'),
      headers: new Headers({ "content-type": "application/json" }),
    })
  ) as unknown as typeof fetch;
}

function Wrapper({ children }: { children: ReactNode }) {
  return (
    <SWRConfig value={{ provider: () => new Map(), dedupingInterval: 0 }}>
      {children}
    </SWRConfig>
  );
}

function renderPage(ui: ReactNode) {
  return render(ui, { wrapper: Wrapper });
}

// ─── Tests ──────────────────────────────────

describe("Page Components", () => {
  beforeEach(() => {
    globalThis.fetch = originalFetch;
  });

  afterEach(() => {
    cleanup();
    globalThis.fetch = originalFetch;
  });

  // ─── ActivityPage ──────────────────────────────────

  describe("ActivityPage", () => {
    const mockActivityData = [
      {
        id: 1,
        timestamp: "2024-01-01T00:00:00Z",
        activity_type: "issue_received",
        source: "linear",
        issue_id: "LIN-123",
        short_id: "abc123",
        message: "New issue received",
        metadata: { key: "value" },
      },
    ];

    test("renders without crashing", async () => {
      mockFetch([]);
      const ActivityPage = (await import("../src/pages/activity")).default;
      renderPage(<ActivityPage />);
      expect(screen.getByText("Activity Log")).toBeDefined();
    });

    test("shows loading state", async () => {
      // Use a fetch that never resolves to keep loading state
      globalThis.fetch = mock(
        () => new Promise(() => {})
      ) as unknown as typeof fetch;
      const ActivityPage = (await import("../src/pages/activity")).default;
      renderPage(<ActivityPage />);
      // Skeleton loaders are rendered as div elements
      expect(screen.getByText("Activity Log")).toBeDefined();
    });

    test("shows data after fetch resolves", async () => {
      mockFetch(mockActivityData);
      const ActivityPage = (await import("../src/pages/activity")).default;
      renderPage(<ActivityPage />);

      await waitFor(() => {
        expect(screen.getByText("issue received")).toBeDefined();
      });
      expect(screen.getByText("New issue received")).toBeDefined();
      expect(screen.getByText("linear")).toBeDefined();
    });

    test("shows error state", async () => {
      mockFetchError();
      const ActivityPage = (await import("../src/pages/activity")).default;
      renderPage(<ActivityPage />);

      await waitFor(() => {
        expect(screen.getByText("Failed to load activity log.")).toBeDefined();
      });
    });
  });

  // ─── AttemptsPage ──────────────────────────────────

  describe("AttemptsPage", () => {
    const mockAttemptsData = {
      attempts: [
        {
          id: 1,
          source: "sentry",
          short_id: "def456",
          title: "Fix bug",
          status: "success",
          pr_url: "https://github.com/test/pr/1",
          attempted_at: "2024-01-01T00:00:00Z",
          retry_count: 0,
        },
      ],
      total: 1,
      page: 1,
      per_page: 20,
    };

    test("renders without crashing", async () => {
      mockFetch(mockAttemptsData);
      const AttemptsPage = (await import("../src/pages/attempts")).default;
      renderPage(<AttemptsPage />);
      expect(screen.getByText("Attempts")).toBeDefined();
    });

    test("shows loading state", async () => {
      globalThis.fetch = mock(
        () => new Promise(() => {})
      ) as unknown as typeof fetch;
      const AttemptsPage = (await import("../src/pages/attempts")).default;
      renderPage(<AttemptsPage />);
      expect(screen.getByText("Attempts")).toBeDefined();
    });

    test("shows data after fetch resolves", async () => {
      mockFetch(mockAttemptsData);
      const AttemptsPage = (await import("../src/pages/attempts")).default;
      renderPage(<AttemptsPage />);

      await waitFor(() => {
        expect(screen.getByText("def456")).toBeDefined();
      });
      expect(screen.getByText("Page 1 of 1 (1 total)")).toBeDefined();
    });

    test("shows error state", async () => {
      mockFetchError();
      const AttemptsPage = (await import("../src/pages/attempts")).default;
      renderPage(<AttemptsPage />);

      await waitFor(() => {
        expect(screen.getByText("Failed to load attempts.")).toBeDefined();
      });
    });

    test("shows attempt detail panel when clicking attempt ID", async () => {
      mockFetchByUrl({
        "/api/attempts/1/detail": {
          attempt: {
            id: 1, issue_id: "SEN-999", short_id: "def456", source: "sentry",
            attempted_at: "2024-01-01T00:00:00Z", pr_url: "https://github.com/test/pr/99",
            github_repo: "test/repo", github_pr_number: 99, status: "success",
            error_message: null, merged_at: null, resolved_at: null,
            retry_count: 1, last_retry_at: null, issue_labels: ["bug", "critical"],
            parent_attempt_id: null, cascade_repo: null,
          },
          executions: [{
            id: 1, attempt_id: 1, started_at: "2024-01-01T00:00:00Z",
            completed_at: "2024-01-01T00:01:00Z", duration_secs: 60.5,
            exit_code: 0, timed_out: false, stdout_preview: null, stderr_preview: null,
            stdout_log_path: null, stderr_log_path: null,
            prompt_used: null, prompt_hash: null, model_version: null,
            working_directory: null, git_branch: null, git_commit_before: null,
            git_commit_after: null, files_changed: 3, lines_added: 45, lines_removed: 12,
          }],
          reviews: [{
            id: 1, attempt_id: 1, pr_url: "https://github.com/test/pr/99",
            reviewer: "alice", review_state: "approved", submitted_at: "2024-01-01T00:00:00Z",
            body: "LGTM", sentiment: "positive", actionable_feedback: "Good fix overall",
          }],
          feedback: {
            id: 1, attempt_id: 1, source: "sentry", issue_id: "SEN-999",
            issue_text: "Timeout error", prompt_used: "fix the timeout",
            outcome: "success", error_type: null, learnings: "Added retry logic",
            keywords: ["timeout", "retry"],
          },
        },
        "/api/attempts": mockAttemptsData,
      });
      const AttemptsPage = (await import("../src/pages/attempts")).default;
      renderPage(<AttemptsPage />);

      await waitFor(() => {
        expect(screen.getByText("def456")).toBeDefined();
      });

      fireEvent.click(screen.getByText("def456"));

      await waitFor(() => {
        expect(screen.getByText("Attempt Detail: def456")).toBeDefined();
      });

      expect(screen.getByText("Executions")).toBeDefined();
      expect(screen.getByText("60.5s")).toBeDefined();
      expect(screen.getByText("+45 / -12")).toBeDefined();

      expect(screen.getByText("Reviews")).toBeDefined();
      expect(screen.getByText("alice")).toBeDefined();
      expect(screen.getByText("Good fix overall")).toBeDefined();

      expect(screen.getByText("Feedback Outcome")).toBeDefined();
      expect(screen.getByText("Added retry logic")).toBeDefined();

      expect(screen.getByText("bug")).toBeDefined();
      expect(screen.getByText("critical")).toBeDefined();
    });
  });

  // ─── AnalyticsPage ──────────────────────────────────

  describe("AnalyticsPage", () => {
    const mockSummary = {
      success_rate: 85.5,
      total_processed: 100,
      total_successful: 85,
      total_merged: 60,
      avg_processing_time_secs: 45.3,
      avg_time_to_merge_hours: 2.1,
      most_common_error: "timeout",
      success_rate_by_source: { linear: 90, sentry: 80 },
    };

    test("renders without crashing", async () => {
      mockFetchByUrl({
        "/api/analytics/summary": mockSummary,
        "/api/metrics": [],
      });
      const AnalyticsPage = (await import("../src/pages/analytics")).default;
      renderPage(<AnalyticsPage />);
      expect(screen.getByText("Analytics")).toBeDefined();
    });

    test("shows loading state", async () => {
      globalThis.fetch = mock(
        () => new Promise(() => {})
      ) as unknown as typeof fetch;
      const AnalyticsPage = (await import("../src/pages/analytics")).default;
      renderPage(<AnalyticsPage />);
      expect(screen.getByText("Analytics")).toBeDefined();
    });

    test("shows data after fetch resolves", async () => {
      mockFetchByUrl({
        "/api/analytics/summary": mockSummary,
        "/api/metrics": [],
      });
      const AnalyticsPage = (await import("../src/pages/analytics")).default;
      renderPage(<AnalyticsPage />);

      await waitFor(() => {
        expect(screen.getByText("85.5%")).toBeDefined();
      });
      expect(screen.getByText("100")).toBeDefined();
    });

    test("shows error state gracefully", async () => {
      // Analytics page does not show an explicit error message,
      // it just shows nothing when data fails. Verify it doesn't crash.
      mockFetchError();
      const AnalyticsPage = (await import("../src/pages/analytics")).default;
      renderPage(<AnalyticsPage />);
      // Page header still renders
      expect(screen.getByText("Analytics")).toBeDefined();
    });
  });

  describe("TelemetryPage", () => {
    const mockOverview = {
      generated_at: "2024-01-01T00:00:00Z",
      uptime_secs: 7200,
      windows: [
        {
          window: "1h",
          processed: 12,
          successful: 10,
          failed: 2,
          merged: 6,
          success_rate: 83.3,
          error_rate: 16.7,
          throughput_per_hour: 12,
        },
      ],
      queue: {
        pending_attempts: 3,
        retryable_attempts: 2,
        ready_retries: 1,
        open_prs: 4,
        watches_awaiting_release: 1,
        watches_monitoring: 2,
        watches_resolved: 5,
        watches_regressed: 1,
      },
      processing_time: {
        all_time: { samples: 10, avg_secs: 30, p50_secs: 20, p95_secs: 60, p99_secs: 80, max_secs: 90 },
        last_24h: { samples: 5, avg_secs: 25, p50_secs: 20, p95_secs: 45, p99_secs: 50, max_secs: 60 },
      },
      source_breakdown: [
        {
          source: "linear",
          total: 10,
          pending: 1,
          success: 5,
          failed: 2,
          merged: 2,
          closed: 0,
          cannot_fix: 0,
          retryable: 1,
          success_rate: 70,
        },
      ],
      top_errors: [],
      activity_last_hour: { processing_completed: 4 },
      metric_counts_last_24h: { processing_time: 5 },
      diagnostics: { fix_attempts: 10 },
      pr_analytics: {
        total: 8,
        open: 4,
        merged: 3,
        closed: 1,
        avg_time_to_first_review_mins: 30,
        avg_time_to_merge_mins: 90,
        avg_review_cycles: 1.2,
        merge_rate: 0.75,
        by_repo: {},
      },
    };

    const mockSeries = {
      period: "day",
      bucket_minutes: 30,
      generated_at: "2024-01-01T00:00:00Z",
      points: [
        {
          bucket_start: "2024-01-01T00:00:00Z",
          total: 3,
          pending: 1,
          success: 1,
          failed: 0,
          merged: 1,
          closed: 0,
          cannot_fix: 0,
        },
      ],
    };

    const mockPipeline = {
      generated_at: "2024-01-01T00:00:00Z",
      period: "day",
      totals: {
        fetched: 20,
        matched: 10,
        queued: 8,
        processed: 6,
        pr_created: 3,
        retries_found: 2,
        retries_executed: 1,
        retries_failed: 0,
        pr_status_checks: 5,
        pr_status_merged: 2,
        pr_status_closed: 1,
        pr_status_errors: 0,
        regression_watches_created: 1,
        auto_resolved_on_merge: 1,
        cascade_triggered: 1,
        cascade_failed: 0,
      },
      conversion: {
        match_rate: 0.5,
        queue_rate: 0.8,
        processing_rate: 0.75,
        pr_yield_rate: 0.5,
      },
      poll_load: {
        poll_cycles: 4,
        avg_cycle_secs: 4.2,
        p95_cycle_secs: 6.8,
        active_avg: 0.5,
        active_max: 2,
        pending_avg: 3,
        pending_max: 5,
        total_latest: 10,
      },
      per_source: [
        {
          source: "linear",
          fetched: 10,
          matched: 6,
          queued: 5,
          processed: 4,
          pr_created: 2,
          retries_executed: 1,
          retries_failed: 0,
          match_rate: 0.6,
          queue_rate: 0.83,
          processing_rate: 0.8,
          pr_yield_rate: 0.5,
        },
      ],
    };

    const mockLatency = {
      generated_at: "2024-01-01T00:00:00Z",
      period: "day",
      overall: {
        samples: 12,
        avg_secs: 32,
        p50_secs: 25,
        p95_secs: 60,
        p99_secs: 75,
        max_secs: 80,
      },
      by_status: [
        {
          status: "merged",
          summary: {
            samples: 6,
            avg_secs: 28,
            p50_secs: 24,
            p95_secs: 55,
            p99_secs: 70,
            max_secs: 72,
          },
        },
      ],
      histogram: [
        { label: "<=15s", upper_bound_secs: 15, count: 2 },
        { label: "<=30s", upper_bound_secs: 30, count: 5 },
        { label: "<=60s", upper_bound_secs: 60, count: 4 },
        { label: ">5m", upper_bound_secs: null, count: 0 },
      ],
    };

    test("renders without crashing", async () => {
      mockFetchByUrl({
        "/api/telemetry/overview": mockOverview,
        "/api/telemetry/timeseries": mockSeries,
        "/api/telemetry/pipeline": mockPipeline,
        "/api/telemetry/latency": mockLatency,
      });
      const TelemetryPage = (await import("../src/pages/telemetry")).default;
      renderPage(<TelemetryPage />);
      expect(screen.getByText("Telemetry")).toBeDefined();
    });

    test("shows telemetry data", async () => {
      mockFetchByUrl({
        "/api/telemetry/overview": mockOverview,
        "/api/telemetry/timeseries": mockSeries,
        "/api/telemetry/pipeline": mockPipeline,
        "/api/telemetry/latency": mockLatency,
      });
      const TelemetryPage = (await import("../src/pages/telemetry")).default;
      renderPage(<TelemetryPage />);

      await waitFor(() => {
        expect(screen.getByText("Pending Attempts")).toBeDefined();
      });
      expect(screen.getByText("Window Performance")).toBeDefined();
      expect(screen.getByText("Poll Pipeline")).toBeDefined();
      expect(screen.getByText("Processing Latency")).toBeDefined();
    });
  });

  // ─── ErrorsPage ──────────────────────────────────

  describe("ErrorsPage", () => {
    const mockErrorsData = [
      {
        id: 1,
        pattern_hash: "abc",
        error_type: "timeout",
        error_message: "Connection timed out",
        first_seen: "2024-01-01T00:00:00Z",
        last_seen: "2024-01-02T00:00:00Z",
        occurrence_count: 15,
        sources: ["sentry"],
        example_issue_ids: ["SEN-1"],
        resolution_hints: "Check network",
      },
    ];

    test("renders without crashing", async () => {
      mockFetch(mockErrorsData);
      const ErrorsPage = (await import("../src/pages/errors")).default;
      renderPage(<ErrorsPage />);
      expect(screen.getByText("Error Patterns")).toBeDefined();
    });

    test("shows loading state", async () => {
      globalThis.fetch = mock(
        () => new Promise(() => {})
      ) as unknown as typeof fetch;
      const ErrorsPage = (await import("../src/pages/errors")).default;
      renderPage(<ErrorsPage />);
      expect(screen.getByText("Error Patterns")).toBeDefined();
    });

    test("shows data after fetch resolves", async () => {
      mockFetch(mockErrorsData);
      const ErrorsPage = (await import("../src/pages/errors")).default;
      renderPage(<ErrorsPage />);

      await waitFor(() => {
        expect(screen.getByText("timeout")).toBeDefined();
      });
      expect(screen.getByText("15")).toBeDefined();
    });

    test("shows error state", async () => {
      mockFetchError();
      const ErrorsPage = (await import("../src/pages/errors")).default;
      renderPage(<ErrorsPage />);

      await waitFor(() => {
        expect(
          screen.getByText("Failed to load error patterns.")
        ).toBeDefined();
      });
    });
  });

  // ─── PrsPage ──────────────────────────────────

  describe("PrsPage", () => {
    const mockAnalytics = {
      total: 50,
      open: 10,
      merged: 30,
      closed: 10,
      avg_time_to_first_review_mins: 30,
      avg_time_to_merge_mins: 120,
      avg_review_cycles: 1.5,
      merge_rate: 60,
      by_repo: {},
    };

    const mockPrs = [
      {
        id: 1,
        pr_url: "https://github.com/test/pr/1",
        github_repo: "test/repo",
        pr_number: 42,
        attempt_id: 1,
        issue_id: "LIN-1",
        issue_source: "linear",
        title: "Fix authentication bug",
        description: null,
        author: "claude",
        head_branch: "fix/auth",
        base_branch: "main",
        status: "merged",
        created_at: "2024-01-01T00:00:00Z",
        updated_at: null,
        merged_at: "2024-01-02T00:00:00Z",
        closed_at: null,
        approvals_count: 2,
        changes_requested_count: 0,
        comments_count: 3,
        last_review_at: null,
        time_to_first_review_mins: 15,
        time_to_merge_mins: 60,
        review_cycles: 1,
        files_changed: 5,
        lines_added: 100,
        lines_removed: 20,
      },
    ];

    test("renders without crashing", async () => {
      mockFetchByUrl({
        "/api/prs/analytics": mockAnalytics,
        "/api/prs?": mockPrs,
      });
      const PrsPage = (await import("../src/pages/prs")).default;
      renderPage(<PrsPage />);
      expect(screen.getByText("Pull Requests")).toBeDefined();
    });

    test("shows loading state", async () => {
      globalThis.fetch = mock(
        () => new Promise(() => {})
      ) as unknown as typeof fetch;
      const PrsPage = (await import("../src/pages/prs")).default;
      renderPage(<PrsPage />);
      expect(screen.getByText("Pull Requests")).toBeDefined();
    });

    test("shows data after fetch resolves", async () => {
      mockFetchByUrl({
        "/api/prs/analytics": mockAnalytics,
        "/api/prs?": mockPrs,
      });
      const PrsPage = (await import("../src/pages/prs")).default;
      renderPage(<PrsPage />);

      await waitFor(() => {
        expect(screen.getByText("50")).toBeDefined();
      });
      expect(screen.getByText("#42")).toBeDefined();
    });

    test("shows error state", async () => {
      mockFetchError();
      const PrsPage = (await import("../src/pages/prs")).default;
      renderPage(<PrsPage />);

      await waitFor(() => {
        expect(
          screen.getByText("Failed to load pull requests.")
        ).toBeDefined();
      });
    });
  });

  // ─── FeedbackPage ──────────────────────────────────

  describe("FeedbackPage", () => {
    const mockFeedbackData = [
      {
        id: 1,
        attempt_id: 42,
        source: "linear",
        issue_id: "LIN-123",
        issue_text: "Bug report",
        prompt_used: "fix this",
        outcome: "success",
        error_type: null,
        learnings: "Always check null values",
        keywords: ["null-check", "validation"],
      },
    ];

    test("renders without crashing", async () => {
      mockFetch(mockFeedbackData);
      const FeedbackPage = (await import("../src/pages/feedback")).default;
      renderPage(<FeedbackPage />);
      expect(screen.getByText("Feedback")).toBeDefined();
    });

    test("shows loading state", async () => {
      globalThis.fetch = mock(
        () => new Promise(() => {})
      ) as unknown as typeof fetch;
      const FeedbackPage = (await import("../src/pages/feedback")).default;
      renderPage(<FeedbackPage />);
      expect(screen.getByText("Feedback")).toBeDefined();
    });

    test("shows data after fetch resolves", async () => {
      mockFetch(mockFeedbackData);
      const FeedbackPage = (await import("../src/pages/feedback")).default;
      renderPage(<FeedbackPage />);

      await waitFor(() => {
        expect(screen.getByText("success")).toBeDefined();
      });
      expect(screen.getByText("null-check")).toBeDefined();
    });

    test("shows error state", async () => {
      mockFetchError();
      const FeedbackPage = (await import("../src/pages/feedback")).default;
      renderPage(<FeedbackPage />);

      await waitFor(() => {
        expect(
          screen.getByText("Failed to load feedback data.")
        ).toBeDefined();
      });
    });
  });

  // ─── RegressionsPage ──────────────────────────────────

  describe("RegressionsPage", () => {
    const mockRegressionsData = [
      {
        id: 1,
        issue_type: "sentry_error",
        issue_id: "SEN-100",
        fix_attempt_id: 5,
        status: "monitoring",
        pr_merged_at: "2024-01-01T00:00:00Z",
        monitoring_started_at: "2024-01-02T00:00:00Z",
        resolved_at: null,
        regressed_at: null,
        created_at: "2024-01-01T00:00:00Z",
      },
    ];

    test("renders without crashing", async () => {
      mockFetch(mockRegressionsData);
      const RegressionsPage = (await import("../src/pages/regressions"))
        .default;
      renderPage(<RegressionsPage />);
      expect(screen.getByText("Regressions")).toBeDefined();
    });

    test("shows tabs", async () => {
      mockFetch(mockRegressionsData);
      const RegressionsPage = (await import("../src/pages/regressions"))
        .default;
      renderPage(<RegressionsPage />);
      expect(screen.getByText("Awaiting Release")).toBeDefined();
      expect(screen.getByText("Monitoring")).toBeDefined();
    });

    test("shows loading state", async () => {
      globalThis.fetch = mock(
        () => new Promise(() => {})
      ) as unknown as typeof fetch;
      const RegressionsPage = (await import("../src/pages/regressions"))
        .default;
      renderPage(<RegressionsPage />);
      expect(screen.getByText("Regressions")).toBeDefined();
    });

    test("shows data after fetch resolves", async () => {
      mockFetch(mockRegressionsData);
      const RegressionsPage = (await import("../src/pages/regressions"))
        .default;
      renderPage(<RegressionsPage />);

      await waitFor(() => {
        expect(screen.getByText("SEN-100")).toBeDefined();
      });
    });

    test("shows error state", async () => {
      mockFetchError();
      const RegressionsPage = (await import("../src/pages/regressions"))
        .default;
      renderPage(<RegressionsPage />);

      await waitFor(() => {
        expect(
          screen.getByText("Failed to load regressions.")
        ).toBeDefined();
      });
    });

    test("shows regression checks timeline when clicking issue ID", async () => {
      mockFetchByUrl({
        "/api/regressions/1/checks": [
          {
            id: 1, regression_watch_id: 1, issue_still_exists: true,
            checked_at: "2024-01-03T00:00:00Z", check_details: "Error still occurring in logs",
            created_at: "2024-01-03T00:00:00Z",
          },
          {
            id: 2, regression_watch_id: 1, issue_still_exists: false,
            checked_at: "2024-01-05T00:00:00Z", check_details: "No more errors found",
            created_at: "2024-01-05T00:00:00Z",
          },
        ],
        "/api/regressions": mockRegressionsData,
      });
      const RegressionsPage = (await import("../src/pages/regressions")).default;
      renderPage(<RegressionsPage />);

      await waitFor(() => {
        expect(screen.getByText("SEN-100")).toBeDefined();
      });

      fireEvent.click(screen.getByText("SEN-100"));

      await waitFor(() => {
        expect(screen.getByText("Regression Checks Timeline")).toBeDefined();
      });

      expect(screen.getByText("Issue persists")).toBeDefined();
      expect(screen.getByText("Issue resolved")).toBeDefined();
      expect(screen.getByText("Error still occurring in logs")).toBeDefined();
      expect(screen.getByText("No more errors found")).toBeDefined();
    });
  });

  // ─── ExperimentsPage ──────────────────────────────────

  describe("ExperimentsPage", () => {
    const mockExperimentsData = [
      {
        id: 1,
        experiment_name: "prompt-v2",
        variant: "control",
        prompt_template: "fix {{issue}}",
        prompt_hash: "abc",
        created_at: "2024-01-01T00:00:00Z",
        active: true,
        success_count: 10,
        failure_count: 2,
        avg_time_to_merge: 45,
        avg_review_score: 4.5,
      },
      {
        id: 2,
        experiment_name: "prompt-v2",
        variant: "treatment",
        prompt_template: "please fix {{issue}}",
        prompt_hash: "def",
        created_at: "2024-01-01T00:00:00Z",
        active: true,
        success_count: 12,
        failure_count: 1,
        avg_time_to_merge: 30,
        avg_review_score: 4.8,
      },
    ];

    test("renders without crashing", async () => {
      mockFetch(mockExperimentsData);
      const ExperimentsPage = (await import("../src/pages/experiments"))
        .default;
      renderPage(<ExperimentsPage />);
      expect(screen.getByText("Experiments")).toBeDefined();
    });

    test("shows loading state", async () => {
      globalThis.fetch = mock(
        () => new Promise(() => {})
      ) as unknown as typeof fetch;
      const ExperimentsPage = (await import("../src/pages/experiments"))
        .default;
      renderPage(<ExperimentsPage />);
      expect(screen.getByText("Experiments")).toBeDefined();
    });

    test("shows data after fetch resolves", async () => {
      mockFetch(mockExperimentsData);
      const ExperimentsPage = (await import("../src/pages/experiments"))
        .default;
      renderPage(<ExperimentsPage />);

      await waitFor(() => {
        expect(screen.getByText("prompt-v2")).toBeDefined();
      });
      expect(screen.getByText("control")).toBeDefined();
      expect(screen.getByText("treatment")).toBeDefined();
      expect(screen.getByText("10")).toBeDefined();
      expect(screen.getByText("12")).toBeDefined();
    });

    test("shows error state", async () => {
      mockFetchError();
      const ExperimentsPage = (await import("../src/pages/experiments"))
        .default;
      renderPage(<ExperimentsPage />);

      await waitFor(() => {
        expect(
          screen.getByText("Failed to load experiments.")
        ).toBeDefined();
      });
    });
  });

  // ─── ReposPage ──────────────────────────────────

  describe("ReposPage", () => {
    const mockRepos = [
      {
        id: 1,
        name: "my-app",
        path: "/home/user/my-app",
        github_url: "https://github.com/user/my-app",
        default_branch: "main",
        file_count: 150,
        last_indexed_at: "2024-01-01T00:00:00Z",
        created_at: "2024-01-01T00:00:00Z",
      },
    ];

    const mockStats = {
      repo_count: 3,
      file_count: 450,
      last_indexed_at: "2024-01-01T00:00:00Z",
    };

    const mockDeps = [
      {
        id: 1,
        upstream: "core-lib",
        downstream: "my-app",
        dep_type: "npm",
        created_at: "2024-01-01T00:00:00Z",
      },
    ];

    test("renders without crashing", async () => {
      mockFetchByUrl({
        "/api/repos/stats": mockStats,
        "/api/repos/dependencies": mockDeps,
        "/api/repos": mockRepos,
      });
      const ReposPage = (await import("../src/pages/repos")).default;
      renderPage(<ReposPage />);
      expect(screen.getByText("Repositories")).toBeDefined();
    });

    test("shows loading state", async () => {
      globalThis.fetch = mock(
        () => new Promise(() => {})
      ) as unknown as typeof fetch;
      const ReposPage = (await import("../src/pages/repos")).default;
      renderPage(<ReposPage />);
      expect(screen.getByText("Repositories")).toBeDefined();
    });

    test("shows data after fetch resolves", async () => {
      mockFetchByUrl({
        "/api/repos/stats": mockStats,
        "/api/repos/dependencies": mockDeps,
        "/api/repos": mockRepos,
      });
      const ReposPage = (await import("../src/pages/repos")).default;
      renderPage(<ReposPage />);

      await waitFor(
        () => {
          expect(screen.getByText("core-lib")).toBeDefined();
        },
        { timeout: 3000 },
      );
      expect(screen.getByText("3")).toBeDefined();
      // "my-app" appears in both repos and deps tables
      expect(screen.getAllByText("my-app").length).toBeGreaterThanOrEqual(1);
    });

    test("shows error state gracefully", async () => {
      mockFetchError();
      const ReposPage = (await import("../src/pages/repos")).default;
      renderPage(<ReposPage />);
      // ReposPage doesn't have explicit error text; verify no crash
      expect(screen.getByText("Repositories")).toBeDefined();
    });
  });

  // ─── InferencePage ──────────────────────────────────

  describe("InferencePage", () => {
    const mockInferenceStats = {
      total_attempts: 50,
      with_feedback: 30,
      correct: 25,
      accuracy: 83.3,
      by_confidence: { high: 20, medium: 15, low: 10, none: 5 },
    };

    const mockHistory = [
      {
        id: 1,
        issue_id: "LIN-500",
        issue_source: "linear",
        extracted_keywords: "auth,login",
        inferred_repo_name: "auth-service",
        confidence: "high",
        inference_reason: "keyword match",
        was_correct: true,
        duration_ms: 120,
        created_at: "2024-01-01T00:00:00Z",
      },
    ];

    test("renders without crashing", async () => {
      mockFetchByUrl({
        "/api/inference/stats": mockInferenceStats,
        "/api/inference/history": mockHistory,
      });
      const InferencePage = (await import("../src/pages/inference")).default;
      renderPage(<InferencePage />);
      expect(screen.getByText("Inference")).toBeDefined();
    });

    test("shows loading state", async () => {
      globalThis.fetch = mock(
        () => new Promise(() => {})
      ) as unknown as typeof fetch;
      const InferencePage = (await import("../src/pages/inference")).default;
      renderPage(<InferencePage />);
      expect(screen.getByText("Inference")).toBeDefined();
    });

    test("shows data after fetch resolves", async () => {
      mockFetchByUrl({
        "/api/inference/stats": mockInferenceStats,
        "/api/inference/history": mockHistory,
      });
      const InferencePage = (await import("../src/pages/inference")).default;
      renderPage(<InferencePage />);

      await waitFor(() => {
        expect(screen.getByText("83.3%")).toBeDefined();
      });
      expect(screen.getByText("LIN-500")).toBeDefined();
      expect(screen.getByText("auth-service")).toBeDefined();
    });

    test("shows error state", async () => {
      mockFetchError();
      const InferencePage = (await import("../src/pages/inference")).default;
      renderPage(<InferencePage />);

      await waitFor(() => {
        expect(
          screen.getByText("Failed to load inference history.")
        ).toBeDefined();
      });
    });
  });
});
