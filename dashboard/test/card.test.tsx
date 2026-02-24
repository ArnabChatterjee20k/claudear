import { afterEach, describe, expect, test } from "bun:test";
import { cleanup, render, screen } from "@testing-library/react";
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "../src/components/ui/card";

afterEach(() => {
  cleanup();
});

describe("Card primitives", () => {
  test("render expected structure and base classes", () => {
    render(
      <Card data-testid="card" className="custom-card">
        <CardHeader data-testid="header" className="custom-header">
          <CardTitle data-testid="title" className="custom-title">
            Dashboard
          </CardTitle>
          <CardDescription data-testid="description" className="custom-description">
            Summary
          </CardDescription>
        </CardHeader>
        <CardContent data-testid="content" className="custom-content">
          Body
        </CardContent>
        <CardFooter data-testid="footer" className="custom-footer">
          Footer
        </CardFooter>
      </Card>
    );

    expect(screen.getByTestId("card").className).toContain("rounded-lg");
    expect(screen.getByTestId("card").className).toContain("custom-card");
    expect(screen.getByTestId("header").className).toContain("p-6");
    expect(screen.getByTestId("header").className).toContain("custom-header");
    expect(screen.getByTestId("title").tagName).toBe("H3");
    expect(screen.getByTestId("title").className).toContain("text-2xl");
    expect(screen.getByTestId("title").className).toContain("custom-title");
    expect(screen.getByTestId("description").tagName).toBe("P");
    expect(screen.getByTestId("description").className).toContain("text-sm");
    expect(screen.getByTestId("description").className).toContain("custom-description");
    expect(screen.getByTestId("content").className).toContain("pt-0");
    expect(screen.getByTestId("content").className).toContain("custom-content");
    expect(screen.getByTestId("footer").className).toContain("items-center");
    expect(screen.getByTestId("footer").className).toContain("custom-footer");
    expect(screen.getByText("Footer")).toBeTruthy();
  });
});
