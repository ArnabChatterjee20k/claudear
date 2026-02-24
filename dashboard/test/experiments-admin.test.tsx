import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { SWRConfig, mutate } from "swr";
import type { ReactNode } from "react";
import { AuthProvider } from "../src/lib/auth";

type ExperimentRecord = {
  id: number;
  experiment_name: string;
  variant: string;
  prompt_template: string;
  prompt_hash: string;
  created_at: string;
  active: boolean;
  success_count: number;
  failure_count: number;
  avg_time_to_merge: number | null;
  avg_review_score: number | null;
};

function Wrapper({ children }: { children: ReactNode }) {
  const provider = () => {
    const cache = new Map();
    cache.set("experiments", []);
    return cache;
  };
  return (
    <SWRConfig value={{ provider, dedupingInterval: 0, revalidateOnMount: true }}>
      <AuthProvider>{children}</AuthProvider>
    </SWRConfig>
  );
}

function createExperimentsFetchHarness(initialExperiments: ExperimentRecord[]) {
  let experiments = [...initialExperiments];
  const calls: Array<{ url: string; method: string; body?: unknown }> = [];
  let failNextPost = false;
  let failNextPut = false;

  globalThis.fetch = mock((input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    const method = init?.method ?? (input instanceof Request ? input.method : "GET");
    const body =
      typeof init?.body === "string"
        ? JSON.parse(init.body)
        : init?.body instanceof FormData
          ? init.body
          : undefined;
    calls.push({ url, method, body });

    if (url.endsWith("/api/auth/me")) {
      return Promise.resolve({
        ok: true,
        status: 200,
        json: () =>
          Promise.resolve({
            id: 1,
            email: "admin@test.com",
            name: "Admin",
            role: "admin",
          }),
      });
    }

    if (url.endsWith("/api/experiments") && method === "GET") {
      return Promise.resolve({
        ok: true,
        status: 200,
        json: () => Promise.resolve(experiments),
      });
    }

    if (url.endsWith("/api/experiments") && method === "POST") {
      if (failNextPost) {
        failNextPost = false;
        return Promise.resolve({ ok: false, status: 500, json: () => Promise.resolve({ error: "fail" }) });
      }
      const payload = body as any;
      const next: ExperimentRecord = {
        id: Math.max(0, ...experiments.map((e) => e.id)) + 1,
        experiment_name: payload.experiment_name,
        variant: payload.variant,
        prompt_template: payload.prompt_template,
        prompt_hash: `hash-${Date.now()}`,
        created_at: "2024-01-01T00:00:00Z",
        active: payload.active ?? true,
        success_count: 0,
        failure_count: 0,
        avg_time_to_merge: null,
        avg_review_score: null,
      };
      experiments = [...experiments, next];
      return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(next) });
    }

    if (url.includes("/api/experiments/") && method === "PUT") {
      if (failNextPut) {
        failNextPut = false;
        return Promise.resolve({ ok: false, status: 500, json: () => Promise.resolve({ error: "fail" }) });
      }
      const id = Number(url.split("/").pop());
      const payload = body as any;
      experiments = experiments.map((exp) =>
        exp.id === id
          ? {
              ...exp,
              experiment_name: payload.experiment_name,
              variant: payload.variant,
              prompt_template: payload.prompt_template,
              active: payload.active,
            }
          : exp
      );
      return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve({ ok: true }) });
    }

    return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve({}) });
  }) as unknown as typeof fetch;

  return {
    calls,
    failNextPost() {
      failNextPost = true;
    },
    failNextPut() {
      failNextPut = true;
    },
  };
}

describe("ExperimentsPage admin flows", () => {
  const originalFetch = globalThis.fetch;
  const originalConfirm = globalThis.confirm;

  beforeEach(async () => {
    await mutate("experiments", undefined, { revalidate: false });
    globalThis.confirm = () => true;
  });

  afterEach(() => {
    cleanup();
    globalThis.fetch = originalFetch;
    globalThis.confirm = originalConfirm;
  });

  test("admin can validate, create, edit, and deactivate experiments", async () => {
    const harness = createExperimentsFetchHarness([]);

    const ExperimentsPage = (await import("../src/pages/experiments")).default;
    render(<ExperimentsPage />, { wrapper: Wrapper });

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Create" })).toBeTruthy();
    });

    fireEvent.click(screen.getByRole("button", { name: "Create" }));
    const createForm = screen.getByRole("button", { name: "Create Experiment" }).closest("form");
    expect(createForm).toBeTruthy();
    if (createForm) {
      fireEvent.submit(createForm);
    }

    await waitFor(() => {
      expect(
        screen.getByText("Experiment name, variant, and prompt template are required.")
      ).toBeTruthy();
    });

    fireEvent.change(screen.getByPlaceholderText("e.g. prompt-quality-v1"), {
      target: { value: "quality-prompt" },
    });
    fireEvent.change(screen.getByPlaceholderText("control"), {
      target: { value: "candidate" },
    });
    fireEvent.change(screen.getByPlaceholderText("Write the prompt template for this variant..."), {
      target: { value: "Candidate prompt template" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create Experiment" }));

    await waitFor(() => {
      expect(screen.getByText("candidate")).toBeTruthy();
    });
    expect(screen.queryByPlaceholderText("e.g. prompt-quality-v1")).toBeNull();

    fireEvent.click(screen.getAllByRole("button", { name: "Edit" })[0]);

    await waitFor(() => {
      expect(screen.getByText("Edit Variant: candidate")).toBeTruthy();
    });

    fireEvent.change(screen.getByDisplayValue("candidate"), { target: { value: "candidate-v2" } });
    fireEvent.change(screen.getByDisplayValue("Candidate prompt template"), {
      target: { value: "Candidate prompt v2" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save Changes" }));

    await waitFor(() => {
      expect(screen.getByText("candidate-v2")).toBeTruthy();
    });
    expect(screen.queryByText("Edit Variant: candidate")).toBeNull();

    fireEvent.click(screen.getAllByRole("button", { name: "Deactivate" })[0]);

    await waitFor(() => {
      const deactivatePut = [...harness.calls]
        .reverse()
        .find((call) => call.method === "PUT" && String(call.url).endsWith("/api/experiments/1"));
      expect(deactivatePut).toBeTruthy();
      expect((deactivatePut?.body as any)?.active).toBe(false);
    });
  });

  test("shows create and edit/deactivate error messages when api calls fail", async () => {
    const harness = createExperimentsFetchHarness([
      {
        id: 1,
        experiment_name: "quality-prompt",
        variant: "control",
        prompt_template: "Base prompt",
        prompt_hash: "hash-1",
        created_at: "2024-01-01T00:00:00Z",
        active: true,
        success_count: 8,
        failure_count: 2,
        avg_time_to_merge: null,
        avg_review_score: null,
      },
    ]);

    const ExperimentsPage = (await import("../src/pages/experiments")).default;
    render(<ExperimentsPage />, { wrapper: Wrapper });

    await waitFor(() => {
      expect(screen.getByText("quality-prompt")).toBeTruthy();
    });

    harness.failNextPost();
    fireEvent.click(screen.getByRole("button", { name: "Create" }));
    fireEvent.change(screen.getByPlaceholderText("e.g. prompt-quality-v1"), {
      target: { value: "quality-prompt" },
    });
    fireEvent.change(screen.getByPlaceholderText("control"), {
      target: { value: "candidate" },
    });
    fireEvent.change(screen.getByPlaceholderText("Write the prompt template for this variant..."), {
      target: { value: "Candidate prompt template" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create Experiment" }));

    await waitFor(() => {
      expect(screen.getByText("Failed to create experiment.")).toBeTruthy();
    });

    fireEvent.click(screen.getAllByRole("button", { name: "Cancel" })[0]);
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));

    await waitFor(() => {
      expect(screen.getByText("Edit Variant: control")).toBeTruthy();
    });

    harness.failNextPut();
    fireEvent.click(screen.getByRole("button", { name: "Save Changes" }));
    await waitFor(() => {
      expect(screen.getByText("Failed to update experiment.")).toBeTruthy();
    });

    harness.failNextPut();
    fireEvent.click(screen.getByRole("button", { name: "Deactivate Variant" }));
    await waitFor(() => {
      expect(screen.getAllByText("Failed to deactivate experiment.").length).toBeGreaterThanOrEqual(1);
    });
  });
});
