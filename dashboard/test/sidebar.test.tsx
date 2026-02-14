import { describe, test, expect, afterEach } from "bun:test";
import { render, screen, cleanup, fireEvent } from "@testing-library/react";
import { Router } from "../src/router";
import { Sidebar } from "../src/components/layout/sidebar";

describe("Sidebar", () => {
  afterEach(() => {
    cleanup();
    window.location.hash = "";
  });

  function renderSidebar(hash = "#/") {
    window.location.hash = hash;
    return render(
      <Router routes={{ [hash.slice(1) || "/"]: () => <Sidebar /> }} />
    );
  }

  test('renders "Claudear" title', () => {
    renderSidebar();
    expect(screen.getByText("Claudear")).toBeDefined();
  });

  test("renders all nav item labels", () => {
    renderSidebar();
    const labels = [
      "Overview",
      "Attempts",
      "PRs",
      "Analytics",
      "Errors",
      "Feedback",
      "Regressions",
      "Experiments",
      "Repos",
      "Inference",
      "Activity",
      "Telemetry",
    ];
    for (const label of labels) {
      expect(screen.getByText(label)).toBeDefined();
    }
  });

  test("active item has correct styling on root path", () => {
    renderSidebar("#/");
    const overviewButton = screen.getByText("Overview").closest("button");
    expect(overviewButton?.className).toContain("bg-primary");
    expect(overviewButton?.className).toContain("font-medium");
  });

  test("non-active items have muted styling", () => {
    renderSidebar("#/");
    const attemptsButton = screen.getByText("Attempts").closest("button");
    expect(attemptsButton?.className).toContain("text-muted-foreground");
  });

  test("clicking a nav item updates the hash", () => {
    renderSidebar("#/");
    const attemptsButton = screen.getByText("Attempts").closest("button");
    fireEvent.click(attemptsButton!);
    expect(window.location.hash).toBe("#/attempts");
  });
});
