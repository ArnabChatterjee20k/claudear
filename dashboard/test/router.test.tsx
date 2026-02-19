import { describe, test, expect, afterEach } from "bun:test";
import { render, screen, cleanup, act } from "@testing-library/react";
import { Router, RouterProvider, useRouter } from "../src/router";

afterEach(() => {
  cleanup();
  window.history.replaceState(null, "", "/");
});

// Helper to test the router hook via a component
function UseRouterHarness() {
  const { path, navigate } = useRouter();
  return (
    <div>
      <span data-testid="path">{path}</span>
      <button onClick={() => navigate("/next")}>go</button>
    </div>
  );
}

describe("RouterProvider", () => {
  test("defaults to '/' when at root", () => {
    window.history.replaceState(null, "", "/");
    render(
      <RouterProvider>
        <UseRouterHarness />
      </RouterProvider>
    );
    expect(screen.getByTestId("path").textContent).toBe("/");
  });

  test("reads path from URL", () => {
    window.history.replaceState(null, "", "/settings");
    render(
      <RouterProvider>
        <UseRouterHarness />
      </RouterProvider>
    );
    expect(screen.getByTestId("path").textContent).toBe("/settings");
  });
});

describe("Router", () => {
  test("renders correct component for path", () => {
    window.history.replaceState(null, "", "/about");
    const routes: Record<string, () => JSX.Element> = {
      "/": () => <div>Home</div>,
      "/about": () => <div>About</div>,
    };
    render(
      <RouterProvider>
        <Router routes={routes} />
      </RouterProvider>
    );
    expect(screen.getByText("About")).toBeTruthy();
  });

  test("falls back to '/' route for unknown paths", () => {
    window.history.replaceState(null, "", "/nonexistent");
    const routes: Record<string, () => JSX.Element> = {
      "/": () => <div>Home</div>,
      "/about": () => <div>About</div>,
    };
    render(
      <RouterProvider>
        <Router routes={routes} />
      </RouterProvider>
    );
    expect(screen.getByText("Home")).toBeTruthy();
  });

  test("renders nothing when no matching route and no '/' route", () => {
    window.history.replaceState(null, "", "/unknown");
    const routes: Record<string, () => JSX.Element> = {
      "/about": () => <div>About</div>,
    };
    const { container } = render(
      <RouterProvider>
        <Router routes={routes} />
      </RouterProvider>
    );
    expect(container.textContent).toBe("");
  });
});

describe("useRouter", () => {
  test("provides path and navigate from context", () => {
    window.history.replaceState(null, "", "/dashboard");

    function Child() {
      const { path, navigate } = useRouter();
      return (
        <div>
          <span data-testid="router-path">{path}</span>
          <button onClick={() => navigate("/other")}>nav</button>
        </div>
      );
    }

    const routes: Record<string, () => JSX.Element> = {
      "/dashboard": () => <Child />,
    };
    render(
      <RouterProvider>
        <Router routes={routes} />
      </RouterProvider>
    );
    expect(screen.getByTestId("router-path").textContent).toBe("/dashboard");
  });
});

describe("Navigation", () => {
  test("calling navigate updates the path via pushState", () => {
    window.history.replaceState(null, "", "/");
    render(
      <RouterProvider>
        <UseRouterHarness />
      </RouterProvider>
    );
    act(() => {
      screen.getByText("go").click();
    });
    expect(window.location.pathname).toBe("/next");
    expect(screen.getByTestId("path").textContent).toBe("/next");
  });
});
