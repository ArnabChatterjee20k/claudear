import { describe, test, expect, afterEach } from "bun:test";
import { render, screen, cleanup, fireEvent } from "@testing-library/react";
import { Router, RouterProvider } from "../src/router";
import { Sidebar } from "../src/components/layout/sidebar";

describe("Sidebar", () => {
  afterEach(() => {
    cleanup();
    window.history.replaceState(null, "", "/");
  });

  function renderSidebar(path = "/") {
    window.history.replaceState(null, "", path);
    return render(
      <RouterProvider>
        <Sidebar />
        <Router routes={{ [path]: () => <div /> }} />
      </RouterProvider>
    );
  }

  test('renders "claudear" title', () => {
    renderSidebar();
    expect(screen.getByText("claudear")).toBeDefined();
  });

  test("renders all nav item labels", () => {
    renderSidebar();
    const labels = [
      "Overview",
      "Issues",
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
    renderSidebar("/");
    const overviewButton = screen.getByText("Overview").closest("button");
    expect(overviewButton?.className).toContain("bg-primary");
    expect(overviewButton?.className).toContain("font-medium");
  });

  test("non-active items have muted styling", () => {
    renderSidebar("/");
    const attemptsButton = screen.getByText("Attempts").closest("button");
    expect(attemptsButton?.className).toContain("text-muted-foreground");
  });

  test("clicking a nav item updates the path", () => {
    renderSidebar("/");
    const attemptsButton = screen.getByText("Attempts").closest("button");
    fireEvent.click(attemptsButton!);
    expect(window.location.pathname).toBe("/attempts");
  });

  test("dark mode toggle adds dark class to documentElement", () => {
    document.documentElement.classList.remove("dark");
    localStorage.removeItem("theme");
    renderSidebar();
    const toggleBtn = screen.getByTitle("Switch to dark mode");
    fireEvent.click(toggleBtn);
    expect(document.documentElement.classList.contains("dark")).toBe(true);
    expect(localStorage.getItem("theme")).toBe("dark");
  });

  test("dark mode toggle removes dark class when already dark", () => {
    document.documentElement.classList.add("dark");
    localStorage.setItem("theme", "dark");
    renderSidebar();
    const toggleBtn = screen.getByTitle("Switch to light mode");
    fireEvent.click(toggleBtn);
    expect(document.documentElement.classList.contains("dark")).toBe(false);
    expect(localStorage.getItem("theme")).toBe("light");
  });
});
